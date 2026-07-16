// AkerNet Bridge — Osiris Daemon Orchestration Layer
// "The guardian of transitions. Every daemon answers to the Bridge."
//
// Responsibilities:
//   - Load The Abyss Network registry via Ma'at
//   - Spawn daemons in dependency order
//   - Listen for daemon registration on Unix socket
//   - Supervise all 42 — restart on failure
//   - Expose HTTP control surface for OPIUM, Anubis, Aker app

use axum::{
    routing::get,
    Router,
    Json,
    extract::State,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::io::{AsyncBufReadExt, BufReader};

use maat::{
    DaemonRegistry,
    DaemonInfo,
    DaemonStatus,
    DaemonMessage,
    BridgeMessage,
    message::Frame,
};

// Unix socket path for daemon ↔ bridge IPC
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const HTTP_PORT:   &str = "0.0.0.0:7474";
const VERSION:     &str = "0.1.2";

/// Shared bridge state — wrapped in Arc<Mutex> for async access
#[derive(Debug)]
pub struct BridgeState {
    /// Live status of all 42 daemons
    pub daemons: HashMap<String, DaemonInfo>,
}

impl BridgeState {
    pub fn new() -> Self {
        let registry = DaemonRegistry::load();
        let mut daemons = HashMap::new();
        for daemon in registry.daemons {
            daemons.insert(daemon.name.clone(), daemon);
        }
        BridgeState { daemons }
    }

    pub fn mark_running(&mut self, name: &str, pid: u32) {
        if let Some(d) = self.daemons.get_mut(name) {
            d.status = DaemonStatus::Running;
            d.pid    = Some(pid);
            tracing::info!("[bridge] {} registered — pid {}", name, pid);
        }
    }

    pub fn mark_failed(&mut self, name: &str) {
        if let Some(d) = self.daemons.get_mut(name) {
            d.status = DaemonStatus::Failed;
            d.pid    = None;
            tracing::warn!("[bridge] {} marked Failed", name);
        }
    }

    pub fn running_names(&self) -> Vec<String> {
        self.daemons
            .values()
            .filter(|d| d.status == DaemonStatus::Running)
            .map(|d| d.name.clone())
            .collect()
    }

    pub fn status_summary(&self) -> String {
        let running = self.daemons.values()
            .filter(|d| d.status == DaemonStatus::Running).count();
        let pending = self.daemons.values()
            .filter(|d| d.status == DaemonStatus::Pending).count();
        let failed  = self.daemons.values()
            .filter(|d| d.status == DaemonStatus::Failed).count();
        format!("running={} pending={} failed={}", running, pending, failed)
    }
}

type SharedState = Arc<Mutex<BridgeState>>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("[bridge] AkerNet Bridge v{} starting", VERSION);

    // Load registry and initialize shared state
    let state: SharedState = Arc::new(Mutex::new(BridgeState::new()));

    // Log the full 42 at boot
    {
        let s = state.lock().unwrap();
        tracing::info!("[bridge] Abyss Network loaded — {} daemons registered", s.daemons.len());
        tracing::info!("[bridge] {}", s.status_summary());
    }

    // Spawn Unix socket listener for daemon registration
    let socket_state = Arc::clone(&state);
    tokio::spawn(async move {
        run_socket_listener(socket_state).await;
    });

    // Spawn daemon orchestrator — spawns daemons in dependency order
    let orchestrator_state = Arc::clone(&state);
    tokio::spawn(async move {
        run_orchestrator(orchestrator_state).await;
    });

    // HTTP control surface
    let app = Router::new()
        .route("/",        get(root))
        .route("/health",  get(health_check))
        .route("/daemons", get(list_daemons))
        .with_state(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind(HTTP_PORT)
        .await
        .expect("[bridge] Failed to bind HTTP port");

    tracing::info!("[bridge] HTTP surface live on {}", HTTP_PORT);
    axum::serve(listener, app)
        .await
        .expect("[bridge] HTTP server failed");
}

/// Unix socket listener — receives DaemonMessage registrations
async fn run_socket_listener(state: SharedState) {
    // Clean up stale socket if it exists
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = match UnixListener::bind(SOCKET_PATH) {
        Ok(l)  => {
            tracing::info!("[bridge] Unix socket listening at {}", SOCKET_PATH);
            l
        }
        Err(e) => {
            tracing::error!("[bridge] Failed to bind Unix socket: {}", e);
            return;
        }
    };

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    handle_daemon_connection(stream, state).await;
                });
            }
            Err(e) => {
                tracing::error!("[bridge] Socket accept error: {}", e);
            }
        }
    }
}

/// Handle a single daemon connection
async fn handle_daemon_connection(
    stream: tokio::net::UnixStream,
    state: SharedState,
) {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        match Frame::decode_daemon_message(&line) {
            Ok(DaemonMessage::Register { name, pid, version }) => {
                tracing::info!(
                    "[bridge] Registration: {} pid={} v{}",
                    name, pid, version
                );
                let mut s = state.lock().unwrap();
                s.mark_running(&name, pid);
            }
            Ok(DaemonMessage::StatusUpdate { name, status }) => {
                tracing::info!("[bridge] Status update: {} → {:?}", name, status);
                if let Some(d) = state.lock().unwrap().daemons.get_mut(&name) {
                    d.status = status;
                }
            }
            Ok(DaemonMessage::Error { name, message }) => {
                tracing::error!("[bridge] Error from {}: {}", name, message);
            }
            Ok(DaemonMessage::Shutdown { name, reason }) => {
                tracing::info!("[bridge] Shutdown: {} — {}", name, reason);
                state.lock().unwrap().mark_failed(&name);
            }
            Err(e) => {
                tracing::warn!("[bridge] Could not decode message: {}", e);
            }
        }
    }
}

/// Orchestrator — spawns daemons in dependency order
/// Runs in a loop, checking what's ready to spawn every tick
async fn run_orchestrator(state: SharedState) {
    tracing::info!("[bridge] Orchestrator started");

    // Give the socket listener a moment to bind
    tokio::time::sleep(Duration::from_millis(200)).await;

    loop {
        let ready: Vec<String> = {
            let s = state.lock().unwrap();
            let running = s.running_names();
            s.daemons
                .values()
                .filter(|d| {
                    d.status == DaemonStatus::Pending
                        && d.is_ready_to_spawn(&running)
                })
                .map(|d| d.name.clone())
                .collect()
        };

        for name in ready {
            spawn_daemon(&name, Arc::clone(&state)).await;
        }

        // Orchestrator tick — check every 500ms
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Attempt to spawn a single daemon binary
async fn spawn_daemon(name: &str, state: SharedState) {
    tracing::info!("[bridge] Spawning: {}", name);

    // Mark as Starting before we attempt spawn
    {
        let mut s = state.lock().unwrap();
        if let Some(d) = s.daemons.get_mut(name) {
            d.status = DaemonStatus::Starting;
        }
    }

    match tokio::process::Command::new(name).spawn() {
        Ok(_child) => {
            tracing::info!("[bridge] Spawned: {}", name);
            // Registration confirmed via Unix socket when daemon calls home
            // If daemon doesn't register within timeout, mark Failed
            let state_clone = Arc::clone(&state);
            let name_owned  = name.to_string();
            tokio::spawn(async move {
                check_registration_timeout(name_owned, state_clone).await;
            });
        }
        Err(e) => {
            tracing::warn!(
                "[bridge] Could not spawn {} — binary not found ({}). \
                 Expected in Stage 2+ when daemon binaries are built.",
                name, e
            );
            // In hosted/proot environment, daemon binaries don't exist yet
            // Mark as Stopped rather than Failed so orchestrator doesn't
            // loop retrying unavailable binaries
            let mut s = state.lock().unwrap();
            if let Some(d) = s.daemons.get_mut(name) {
                d.status = DaemonStatus::Stopped;
            }
        }
    }
}

/// If a daemon doesn't register within 5 seconds of spawning, mark Failed
async fn check_registration_timeout(name: String, state: SharedState) {
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mut s = state.lock().unwrap();
    if let Some(d) = s.daemons.get_mut(&name) {
        if d.status == DaemonStatus::Starting {
            tracing::warn!("[bridge] Registration timeout: {}", name);
            d.status = DaemonStatus::Failed;
        }
    }
}

// ── HTTP Handlers ──────────────────────────────────────────────────────────

async fn root() -> &'static str {
    "AkerNet Bridge — The Guardian is awake."
}

#[derive(Serialize)]
struct HealthResponse {
    status:   String,
    service:  String,
    version:  String,
    daemons:  String,
}

async fn health_check(
    State(state): State<SharedState>,
) -> Json<HealthResponse> {
    let s = state.lock().unwrap();
    Json(HealthResponse {
        status:  "ok".to_string(),
        service: "akernet-bridge".to_string(),
        version: VERSION.to_string(),
        daemons: s.status_summary(),
    })
}

#[derive(Serialize)]
struct DaemonSummary {
    id:     u8,
    name:   String,
    domain: String,
    status: String,
    pid:    Option<u32>,
}

async fn list_daemons(
    State(state): State<SharedState>,
) -> Json<Vec<DaemonSummary>> {
    let s = state.lock().unwrap();
    let mut list: Vec<DaemonSummary> = s.daemons
        .values()
        .map(|d| DaemonSummary {
            id:     d.id,
            name:   d.name.clone(),
            domain: d.domain.as_str().to_string(),
            status: d.status.as_str().to_string(),
            pid:    d.pid,
        })
        .collect();
    // Sort by ID so output is always ordered 1→42
    list.sort_by_key(|d| d.id);
    Json(list)
}

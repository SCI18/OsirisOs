// AkerNet Bridge — Osiris Daemon Orchestration Layer
// "The guardian of transitions. Every daemon answers to the Bridge."
//
// Responsibilities:
//   - Load The Abyss Network registry via Ma'at
//   - Spawn daemons in dependency order
//   - Listen for daemon registration on Unix socket, verified via SO_PEERCRED
//   - Supervise all 42 — restart on failure with backoff
//   - Forward daemon messages to logd for durable logging
//   - Expose HTTP control surface for OPIUM, Anubis, Aker app

use axum::{
    routing::get,
    Router,
    Json,
    extract::State,
};
use serde::Serialize;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use maat::{
    DaemonRegistry,
    DaemonStatus,
    DaemonMessage,
    BridgeMessage,
    message::Frame,
};

const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const HTTP_PORT:   &str = "0.0.0.0:7474";
const VERSION:     &str = "0.1.3";

// FIX (fpo.md Finding 1): restart ceiling and backoff base for daemon
// restart-on-failure, using the previously-unused restart_count field.
const MAX_DAEMON_RESTARTS: u32 = 3;
const RESTART_BACKOFF_BASE_MS: u64 = 1000;

/// FIX (fpo.md Finding 4): daemon binaries are resolved relative to this
/// directory rather than relying on PATH. Configurable via
/// OSIRIS_DAEMON_BIN_DIR; falls back to a bare name (old PATH-based
/// behavior) if unset, so this doesn't break environments that do have
/// daemons on PATH already.
fn daemon_bin_dir() -> Option<String> {
    std::env::var("OSIRIS_DAEMON_BIN_DIR").ok()
}

fn resolve_daemon_path(name: &str) -> String {
    match daemon_bin_dir() {
        Some(dir) => format!("{}/{}", dir.trim_end_matches('/'), name),
        None => name.to_string(),
    }
}

/// Shared bridge state.
///
/// FIX (fpo.md Finding 5): wraps `DaemonRegistry` directly instead of
/// duplicating its data into a separate HashMap — registry is now the
/// single source of truth, `ready_to_spawn`/`is_healthy`/`status_summary`/
/// `running_names` all delegate to it.
///
/// FIX (fpo.md Finding 2): retains real `Child` handles in a *separate*
/// map, not inside `DaemonInfo` — `DaemonInfo` derives Serialize (used by
/// the /daemons HTTP endpoint) and `tokio::process::Child` does not
/// implement Serialize, so it cannot live on that struct.
pub struct BridgeState {
    pub registry: DaemonRegistry,
    pub children: std::collections::HashMap<String, tokio::process::Child>,
    /// Write half of logd's connection, held once logd registers, so Bridge
    /// can forward other daemons' messages to it (approved in nsp.md).
    pub logd_write_half: Option<tokio::net::unix::OwnedWriteHalf>,
}

impl BridgeState {
    pub fn new() -> Self {
        BridgeState {
            registry: DaemonRegistry::load(),
            children: std::collections::HashMap::new(),
            logd_write_half: None,
        }
    }

    pub fn mark_running(&mut self, name: &str, pid: u32) {
        if let Some(d) = self.registry.get_mut(name) {
            d.status = DaemonStatus::Running;
            d.pid = Some(pid);
            d.restart_count = 0; // successful registration resets the counter
            tracing::info!("[bridge] {} registered — pid {}", name, pid);
        }
    }

    pub fn mark_failed(&mut self, name: &str) {
        if let Some(d) = self.registry.get_mut(name) {
            d.status = DaemonStatus::Failed;
            d.pid = None;
            tracing::warn!("[bridge] {} marked Failed", name);
        }
        self.children.remove(name);
    }
}

type SharedState = Arc<Mutex<BridgeState>>;

/// FIX (fpo.md Finding 3/8): retrieve the kernel-verified credentials of
/// the process on the other end of a Unix socket via SO_PEERCRED. This is
/// filled in by the kernel at connection time and cannot be spoofed by the
/// connecting process.
fn get_peer_pid(stream: &UnixStream) -> Option<i32> {
    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    if result == 0 {
        Some(cred.pid)
    } else {
        None
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("[bridge] AkerNet Bridge v{} starting", VERSION);

    let state: SharedState = Arc::new(Mutex::new(BridgeState::new()));

    {
        let s = state.lock().await;
        tracing::info!("[bridge] Abyss Network loaded — {} daemons registered", s.registry.daemons.len());
        tracing::info!("[bridge] {}", s.registry.status_summary());
    }

    let socket_state = Arc::clone(&state);
    tokio::spawn(async move {
        run_socket_listener(socket_state).await;
    });

    let orchestrator_state = Arc::clone(&state);
    tokio::spawn(async move {
        run_orchestrator(orchestrator_state).await;
    });

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

async fn run_socket_listener(state: SharedState) {
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = match UnixListener::bind(SOCKET_PATH) {
        Ok(l) => {
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

/// Handle a single daemon connection. Verifies the claimed PID against the
/// kernel-attested peer credential before trusting the registration.
async fn handle_daemon_connection(
    stream: UnixStream,
    state: SharedState,
) {
    let peer_pid = get_peer_pid(&stream);

    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let mut verified_name: Option<String> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        match Frame::decode_daemon_message(&line) {
            Ok(DaemonMessage::Register { name, pid, version }) => {
                // FIX (fpo.md Finding 3/8): reject if claimed pid doesn't
                // match the kernel-verified peer pid.
                match peer_pid {
                    Some(real_pid) if real_pid as u32 == pid => {
                        tracing::info!(
                            "[bridge] Registration: {} pid={} v{} (peer-verified)",
                            name, pid, version
                        );
                        {
                            let mut s = state.lock().await;
                            s.mark_running(&name, pid);
                        }
                        verified_name = Some(name.clone());

                        let ack = BridgeMessage::Acknowledged { name: name.clone() };
                        if let Ok(frame) = Frame::encode(&ack) {
                            let _ = write_half.write_all(frame.as_bytes()).await;
                        }

                        // Hold logd's write half for Forward routing.
                        if name == "logd" {
                            let mut s = state.lock().await;
                            s.logd_write_half = Some(write_half);
                            // write_half moved; this connection's send path
                            // now goes exclusively through logd_write_half.
                            return;
                        }
                    }
                    Some(real_pid) => {
                        tracing::warn!(
                            "[bridge] REJECTED registration: '{}' claimed pid={} but real peer pid={}",
                            name, pid, real_pid
                        );
                        let rejection = BridgeMessage::RegistrationRejected {
                            name: name.clone(),
                            reason: format!("claimed pid {} does not match verified peer pid {}", pid, real_pid),
                        };
                        if let Ok(frame) = Frame::encode(&rejection) {
                            let _ = write_half.write_all(frame.as_bytes()).await;
                        }
                        return; // close connection, do not process further messages
                    }
                    None => {
                        tracing::warn!("[bridge] Could not verify peer credentials for '{}' — rejecting", name);
                        let rejection = BridgeMessage::RegistrationRejected {
                            name: name.clone(),
                            reason: "unable to verify peer credentials".to_string(),
                        };
                        if let Ok(frame) = Frame::encode(&rejection) {
                            let _ = write_half.write_all(frame.as_bytes()).await;
                        }
                        return;
                    }
                }
            }
            Ok(other_msg) => {
                // Any non-Register message from an unverified connection is
                // ignored — only a successfully verified Register grants
                // standing to send further messages.
                let Some(ref name) = verified_name else {
                    tracing::warn!("[bridge] Ignoring message from unregistered connection: {:?}", other_msg);
                    continue;
                };

                match &other_msg {
                    DaemonMessage::StatusUpdate { name: n, status } => {
                        tracing::info!("[bridge] Status update: {} → {:?}", n, status);
                        let mut s = state.lock().await;
                        if let Some(d) = s.registry.get_mut(n) {
                            d.status = status.clone();
                        }
                    }
                    DaemonMessage::Error { name: n, message } => {
                        tracing::error!("[bridge] Error from {}: {}", n, message);
                    }
                    DaemonMessage::Shutdown { name: n, reason } => {
                        tracing::info!("[bridge] Shutdown: {} — {}", n, reason);
                        let mut s = state.lock().await;
                        s.mark_failed(n);
                    }
                    DaemonMessage::Alert { name: n, severity, payload, timestamp } => {
                        tracing::warn!(
                            "[bridge] ALERT [{}] from {} at {}: {}",
                            severity.as_str(), n, timestamp, payload
                        );
                    }
                    DaemonMessage::Register { .. } => unreachable!(),
                }

                // Forward to logd if it's connected and this isn't logd's
                // own connection (logd doesn't need to forward to itself).
                if name != "logd" {
                    let mut s = state.lock().await;
                    if let Some(logd_writer) = s.logd_write_half.as_mut() {
                        let fwd = BridgeMessage::Forward(other_msg);
                        if let Ok(frame) = Frame::encode(&fwd) {
                            if logd_writer.write_all(frame.as_bytes()).await.is_err() {
                                tracing::warn!("[bridge] Failed to forward message to logd, dropping connection reference");
                                s.logd_write_half = None;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("[bridge] Could not decode message: {}", e);
            }
        }
    }

    // Connection closed without an explicit Shutdown message.
    if let Some(name) = verified_name {
        if name != "logd" {
            let mut s = state.lock().await;
            s.mark_failed(&name);
        }
    }
}

/// Orchestrator — spawns daemons in dependency order, and now also retries
/// failed/stopped restartable daemons up to a ceiling (fpo.md Finding 1).
async fn run_orchestrator(state: SharedState) {
    tracing::info!("[bridge] Orchestrator started");
    tokio::time::sleep(Duration::from_millis(200)).await;

    loop {
        let to_spawn: Vec<String> = {
            let s = state.lock().await;
            let running = s.registry.running_names();
            s.registry.ready_to_spawn(&running).iter().map(|d| d.name.clone()).collect()
        };

        for name in to_spawn {
            spawn_daemon(&name, Arc::clone(&state)).await;
        }

        // FIX (fpo.md Finding 1): actually restart daemons that have
        // failed/stopped, respecting restart_count and the ceiling, with
        // exponential backoff proportional to how many times we've already
        // tried.
        let to_restart: Vec<(String, u32)> = {
            let s = state.lock().await;
            s.registry.ready_to_restart(MAX_DAEMON_RESTARTS)
                .iter()
                .map(|d| (d.name.clone(), d.restart_count))
                .collect()
        };

        for (name, restart_count) in to_restart {
            {
                let mut s = state.lock().await;
                if let Some(d) = s.registry.get_mut(&name) {
                    d.status = DaemonStatus::Restarting;
                    d.restart_count += 1;
                }
            }
            let backoff = RESTART_BACKOFF_BASE_MS * 2u64.pow(restart_count.min(6));
            tracing::info!("[bridge] Restarting {} (attempt {}) after {}ms backoff", name, restart_count + 1, backoff);
            tokio::time::sleep(Duration::from_millis(backoff)).await;
            spawn_daemon(&name, Arc::clone(&state)).await;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Attempt to spawn a single daemon binary, retaining its Child handle.
async fn spawn_daemon(name: &str, state: SharedState) {
    let path = resolve_daemon_path(name);
    tracing::info!("[bridge] Spawning: {} ({})", name, path);

    {
        let mut s = state.lock().await;
        if let Some(d) = s.registry.get_mut(name) {
            d.status = DaemonStatus::Starting;
        }
    }

    match tokio::process::Command::new(&path).spawn() {
        Ok(child) => {
            tracing::info!("[bridge] Spawned: {} (pid={:?})", name, child.id());
            {
                let mut s = state.lock().await;
                s.children.insert(name.to_string(), child);
            }
            let state_clone = Arc::clone(&state);
            let name_owned = name.to_string();
            tokio::spawn(async move {
                check_registration_timeout(name_owned, state_clone).await;
            });
        }
        Err(e) => {
            tracing::warn!(
                "[bridge] Could not spawn {} at {} — binary not found ({}). \
                 Set OSIRIS_DAEMON_BIN_DIR if daemons aren't on PATH.",
                name, path, e
            );
            let mut s = state.lock().await;
            if let Some(d) = s.registry.get_mut(name) {
                d.status = DaemonStatus::Stopped;
            }
        }
    }
}

async fn check_registration_timeout(name: String, state: SharedState) {
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mut s = state.lock().await;
    if let Some(d) = s.registry.get_mut(&name) {
        if d.status == DaemonStatus::Starting {
            tracing::warn!("[bridge] Registration timeout: {}", name);
            d.status = DaemonStatus::Failed;
            s.children.remove(&name);
        }
    }
}

// ── HTTP Handlers ──────────────────────────────────────────────────────────

async fn root() -> &'static str {
    "AkerNet Bridge — The Guardian is awake."
}

#[derive(Serialize)]
struct HealthResponse {
    status:  String,
    service: String,
    version: String,
    daemons: String,
}

async fn health_check(State(state): State<SharedState>) -> Json<HealthResponse> {
    let s = state.lock().await;
    Json(HealthResponse {
        status: "ok".to_string(),
        service: "akernet-bridge".to_string(),
        version: VERSION.to_string(),
        daemons: s.registry.status_summary(),
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

async fn list_daemons(State(state): State<SharedState>) -> Json<Vec<DaemonSummary>> {
    let s = state.lock().await;
    let mut list: Vec<DaemonSummary> = s.registry.daemons
        .iter()
        .map(|d| DaemonSummary {
            id: d.id,
            name: d.name.clone(),
            domain: d.domain.as_str().to_string(),
            status: d.status.as_str().to_string(),
            pid: d.pid,
        })
        .collect();
    list.sort_by_key(|d| d.id);
    Json(list)
}

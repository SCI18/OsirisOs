// logd — Assessor #2, SystemCore
// Unified system logging. Ring buffer + hybrid persistence.
// "What is spoken, is weighed. What is weighed, endures."

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus};
use serde_json::Value;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::UnixStream;
use tokio::io::AsyncWrite;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{Duration, interval};
use tracing::{info, warn, error, debug};
use chrono::DateTime;

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const RING_BUFFER_MAX_BYTES: usize = 8 * 1024 * 1024; // 8MB
const PERSIST_PATH: &str = "/var/log/osiris/logd.ring";
const BOOTSTRAP_PATH: &str = "/var/log/osiris/bootstrap.log";
const FLUSH_INTERVAL_MS: u64 = 1000;
const REGISTRATION_TIMEOUT_MS: u64 = 5000;

/// Ring buffer entry
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LogEntry {
    timestamp: String,
    daemon: String,
    level: String,
    message: String,
    fields: Option<Value>,
}

/// Internal channel message: either a bridge message to handle, or a shutdown signal
enum ReaderEvent {
    Bridge(BridgeMessage),
    Closed,
}

/// Logd state
struct Logd {
    ring_buffer: Arc<Mutex<VecDeque<LogEntry>>>,
    ring_buffer_bytes: Arc<Mutex<usize>>,
    persist_writer: Arc<Mutex<Option<BufWriter<tokio::fs::File>>>>,
    bootstrap_writer: Arc<Mutex<Option<BufWriter<tokio::fs::File>>>>,
    socket_write_half: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    daemon_name: String,
    pid: u32,
    /// FIX: bootstrap buffer is meant specifically for pre-registration
    /// logging (logs that occur before Bridge acknowledges registration).
    /// Previously every entry was written to both persist and bootstrap
    /// files unconditionally, making "bootstrap" just a permanent duplicate
    /// log rather than an early-boot capture. This flag gates it correctly.
    registered: Arc<AtomicBool>,
}

impl Logd {
    fn new() -> Result<Self> {
        std::fs::create_dir_all("/var/log/osiris")?;

        Ok(Self {
            ring_buffer: Arc::new(Mutex::new(VecDeque::new())),
            ring_buffer_bytes: Arc::new(Mutex::new(0)),
            persist_writer: Arc::new(Mutex::new(None)),
            bootstrap_writer: Arc::new(Mutex::new(None)),
            socket_write_half: Arc::new(Mutex::new(None)),
            daemon_name: "logd".to_string(),
            pid: std::process::id(),
            registered: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn open_persist_files(&self) -> Result<()> {
        let persist_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(PERSIST_PATH)
            .await?;
        *self.persist_writer.lock().await = Some(BufWriter::new(persist_file));

        let bootstrap_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(BOOTSTRAP_PATH)
            .await?;
        *self.bootstrap_writer.lock().await = Some(BufWriter::new(bootstrap_file));

        info!("[logd] Persistence files opened");
        Ok(())
    }

    /// Connect to Bridge, split the stream into read/write halves, and spawn
    /// a persistent background task that owns the read half for the lifetime
    /// of the connection.
    ///
    /// FIX: the previous design re-created a BufReader and looped
    /// `read_line` inside a function called fresh on every `flush_interval`
    /// tick within `tokio::select!`. Since that function's internal loop
    /// only returns on EOF or error, whichever branch of `select!` completed
    /// first would cause the *other* in-progress branch's future to be
    /// dropped when the loop iterated — silently cancelling a partially-read
    /// line if the socket read was mid-flight when the flush timer fired.
    /// A persistent task with its own loop, decoupled from the main
    /// select loop, avoids this entirely.
    async fn connect_to_bridge(&self) -> Result<mpsc::Receiver<ReaderEvent>> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let (read_half, write_half) = stream.into_split();
        *self.socket_write_half.lock().await = Some(write_half);
        info!("[logd] Connected to AkerNet Bridge at {}", SOCKET_PATH);

        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        let _ = tx.send(ReaderEvent::Closed).await;
                        break;
                    }
                    Ok(_) => {
                        if let Ok(bridge_msg) = Frame::decode_bridge_message(&line) {
                            if tx.send(ReaderEvent::Bridge(bridge_msg)).await.is_err() {
                                break; // receiver dropped, shut down reader
                            }
                        }
                    }
                    Err(e) => {
                        debug!("[logd] Socket read error: {}", e);
                        let _ = tx.send(ReaderEvent::Closed).await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn register(&self) -> Result<()> {
        let msg = DaemonMessage::Register {
            name: self.daemon_name.clone(),
            pid: self.pid,
            version: VERSION.to_string(),
        };
        self.send_frame(msg).await?;
        debug!("[logd] Registration sent");
        Ok(())
    }

    async fn send_frame(&self, msg: DaemonMessage) -> Result<()> {
        let frame = Frame::encode(&msg).map_err(|e| anyhow::anyhow!(e))?;
        let mut guard = self.socket_write_half.lock().await;
        if let Some(writer) = guard.as_mut() {
            writer.write_all(frame.as_bytes()).await?;
            writer.flush().await?;
        }
        Ok(())
    }

    async fn handle_bridge_message(&self, msg: BridgeMessage) -> Result<()> {
        match msg {
            BridgeMessage::Acknowledged { name } => {
                info!("[logd] Registration acknowledged by Bridge: {}", name);
                // FIX: this is the actual, correct trigger for "registered" —
                // flip the flag here so bootstrap-only logging stops once
                // we're truly live, instead of writing to both files forever.
                self.registered.store(true, Ordering::SeqCst);
            }
            BridgeMessage::RegistrationRejected { name, reason } => {
                error!("[logd] Registration rejected: {} — {}", name, reason);
                std::process::exit(1);
            }
            BridgeMessage::StatusRequest => {
                let status = DaemonMessage::StatusUpdate {
                    name: self.daemon_name.clone(),
                    status: DaemonStatus::Running,
                };
                self.send_frame(status).await?;
            }
            BridgeMessage::Stop => {
                info!("[logd] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[logd] Received Reload from Bridge");
            }
            BridgeMessage::Restart => {
                info!("[logd] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Forward(daemon_msg) => {
                // Handle forwarded daemon messages (Alert, Error, StatusUpdate, Register, Shutdown)
                if let Err(e) = self.ingest_daemon_message(daemon_msg).await {
                    error!("[logd] Failed to ingest forwarded daemon message: {}", e);
                }
            }
        }
        Ok(())
    }

    async fn write_log(&self, entry: LogEntry) -> Result<()> {
        let entry_json = serde_json::to_string(&entry)?;
        let entry_size = entry_json.len();

        {
            let mut ring = self.ring_buffer.lock().await;
            let mut bytes = self.ring_buffer_bytes.lock().await;

            while *bytes + entry_size > RING_BUFFER_MAX_BYTES {
                if let Some(old) = ring.pop_front() {
                    let old_size = serde_json::to_string(&old)?.len();
                    *bytes = bytes.saturating_sub(old_size);
                } else {
                    break;
                }
            }
            ring.push_back(entry.clone());
            *bytes += entry_size;
        }

        // FIX: bootstrap file now only receives entries logged *before*
        // Bridge acknowledges registration — i.e. genuine early-boot logs
        // that would otherwise have nowhere durable to land if logd itself
        // isn't fully online yet. Once registered, only the main ring
        // buffer + persist file are written, so bootstrap.log stops
        // growing forever as a silent duplicate of everything.
        if self.registered.load(Ordering::SeqCst) {
            let mut persist = self.persist_writer.lock().await;
            if let Some(writer) = persist.as_mut() {
                writer.write_all(entry_json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
        } else {
            let mut bootstrap = self.bootstrap_writer.lock().await;
            if let Some(writer) = bootstrap.as_mut() {
                writer.write_all(entry_json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
        }

        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        let mut persist = self.persist_writer.lock().await;
        if let Some(writer) = persist.as_mut() {
            writer.flush().await?;
        }
        let mut bootstrap = self.bootstrap_writer.lock().await;
        if let Some(writer) = bootstrap.as_mut() {
            writer.flush().await?;
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[logd] Shutting down gracefully");
        let msg = DaemonMessage::Shutdown {
            name: self.daemon_name.clone(),
            reason: "normal shutdown".to_string(),
        };
        self.send_frame(msg).await?;
        self.flush().await?;
        Ok(())
    }

    /// Ingest a log entry describing another daemon's message.
    ///
    /// Handles forwarded daemon messages (Alert, Error, StatusUpdate, Register, Shutdown)
    /// from the Bridge via the `BridgeMessage::Forward` variant.
    async fn ingest_daemon_message(&self, msg: DaemonMessage) -> Result<()> {
        match msg {
            DaemonMessage::Alert { name, severity, payload, timestamp } => {
                let entry = LogEntry {
                    timestamp,
                    daemon: name,
                    level: severity.as_str().to_string(),
                    message: payload.get("message").and_then(|v| v.as_str()).unwrap_or("Alert").to_string(),
                    fields: Some(payload),
                };
                self.write_log(entry).await?;
            }
            DaemonMessage::Error { name, message } => {
                let entry = LogEntry {
                    timestamp: now_rfc3339(),
                    daemon: name,
                    level: "error".to_string(),
                    message,
                    fields: None,
                };
                self.write_log(entry).await?;
            }
            DaemonMessage::StatusUpdate { name, status } => {
                let entry = LogEntry {
                    timestamp: now_rfc3339(),
                    daemon: name,
                    level: "info".to_string(),
                    message: format!("Status changed to {:?}", status),
                    fields: None,
                };
                self.write_log(entry).await?;
            }
            DaemonMessage::Register { name, pid, version } => {
                let entry = LogEntry {
                    timestamp: now_rfc3339(),
                    daemon: name,
                    level: "info".to_string(),
                    message: format!("Daemon registered (pid={}, v={})", pid, version),
                    fields: None,
                };
                self.write_log(entry).await?;
            }
            DaemonMessage::Shutdown { name, reason } => {
                let entry = LogEntry {
                    timestamp: now_rfc3339(),
                    daemon: name,
                    level: "info".to_string(),
                    message: format!("Daemon shutdown: {}", reason),
                    fields: None,
                };
                self.write_log(entry).await?;
            }
        }
        Ok(())
    }
}

fn now_rfc3339() -> String {
    DateTime::from_timestamp(
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
        0
    ).map(|dt| dt.to_rfc3339()).unwrap_or_default()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("[logd] Osiris Log Daemon v{} starting", VERSION);

    let logd = Logd::new()?;
    logd.open_persist_files().await?;
    let mut reader_rx = logd.connect_to_bridge().await?;
    logd.register().await?;

    // Wait for acknowledgment with timeout by draining the reader channel
    // directly, rather than re-invoking a read function repeatedly.
    let _ = tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        while let Some(event) = reader_rx.recv().await {
            match event {
                ReaderEvent::Bridge(msg) => {
                    let is_ack = matches!(msg, BridgeMessage::Acknowledged { .. });
                    if let Err(e) = logd.handle_bridge_message(msg).await {
                        debug!("[logd] Bridge message handler error: {}", e);
                    }
                    if is_ack {
                        break;
                    }
                }
                ReaderEvent::Closed => break,
            }
        }
    }).await;

    info!("[logd] Registration complete, entering main loop");

    let mut flush_interval = interval(Duration::from_millis(FLUSH_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = flush_interval.tick() => {
                if let Err(e) = logd.flush().await {
                    error!("[logd] Flush error: {}", e);
                }
            }
            event = reader_rx.recv() => {
                match event {
                    Some(ReaderEvent::Bridge(msg)) => {
                        if let Err(e) = logd.handle_bridge_message(msg).await {
                            debug!("[logd] Bridge message handler error: {}", e);
                        }
                    }
                    Some(ReaderEvent::Closed) | None => {
                        warn!("[logd] Bridge connection closed, exiting");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

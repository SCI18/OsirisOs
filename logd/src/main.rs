// logd — Assessor #2, SystemCore
// Unified system logging. Ring buffer + hybrid persistence.
// "What is spoken, is weighed. What is weighed, endures."

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus};
use serde_json::Value;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
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

/// Logd state
struct Logd {
    ring_buffer: Arc<Mutex<VecDeque<LogEntry>>>,
    ring_buffer_bytes: Arc<Mutex<usize>>,
    persist_writer: Arc<Mutex<Option<BufWriter<File>>>>,
    bootstrap_writer: Arc<Mutex<Option<BufWriter<File>>>>,
    socket: Arc<Mutex<Option<UnixStream>>>,
    daemon_name: String,
    pid: u32,
}

impl Logd {
    fn new() -> Result<Self> {
        // Ensure log directory exists
        std::fs::create_dir_all("/var/log/osiris")?;

        Ok(Self {
            ring_buffer: Arc::new(Mutex::new(VecDeque::new())),
            ring_buffer_bytes: Arc::new(Mutex::new(0)),
            persist_writer: Arc::new(Mutex::new(None)),
            bootstrap_writer: Arc::new(Mutex::new(None)),
            socket: Arc::new(Mutex::new(None)),
            daemon_name: "logd".to_string(),
            pid: std::process::id(),
        })
    }

    async fn open_persist_files(&self) -> Result<()> {
        // Open persistence file (append mode)
        let persist_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(PERSIST_PATH)
            .await?;
        let persist_writer = BufWriter::new(persist_file);
        *self.persist_writer.lock().await = Some(persist_writer);

        // Open bootstrap file (append mode)
        let bootstrap_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(BOOTSTRAP_PATH)
            .await?;
        let bootstrap_writer = BufWriter::new(bootstrap_file);
        *self.bootstrap_writer.lock().await = Some(bootstrap_writer);

        info!("[logd] Persistence files opened");
        Ok(())
    }

    async fn connect_to_bridge(&self) -> Result<()> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        *self.socket.lock().await = Some(stream);
        info!("[logd] Connected to AkerNet Bridge at {}", SOCKET_PATH);
        Ok(())
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
        let mut socket_guard = self.socket.lock().await;
        if let Some(stream) = socket_guard.as_mut() {
            stream.write_all(frame.as_bytes()).await?;
            stream.flush().await?;
        }
        Ok(())
    }

    async fn handle_bridge_messages(&self) -> Result<()> {
        // We need to read from the socket without holding the lock for the entire read
        let mut socket_guard = self.socket.lock().await;
        if let Some(stream) = socket_guard.as_mut() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        if let Ok(bridge_msg) = Frame::decode_bridge_message(&line) {
                            self.handle_bridge_message(bridge_msg).await?;
                        }
                        line.clear();
                    }
                    Err(e) => {
                        debug!("[logd] Socket read error: {}", e);
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_bridge_message(&self, msg: BridgeMessage) -> Result<()> {
        match msg {
            BridgeMessage::Acknowledged { name } => {
                info!("[logd] Registration acknowledged by Bridge: {}", name);
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
        }
        Ok(())
    }

    async fn write_log(&self, entry: LogEntry) -> Result<()> {
        let entry_json = serde_json::to_string(&entry)?;
        let entry_size = entry_json.len();

        // Add to ring buffer
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

        // Persist to disk
        {
            let mut persist = self.persist_writer.lock().await;
            if let Some(writer) = persist.as_mut() {
                writer.write_all(entry_json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
        }

        // Also write to bootstrap (for early boot logs)
        {
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

    /// Ingest a log entry from another daemon (via Bridge)
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
                    timestamp: DateTime::from_timestamp(
                        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                        0
                    ).map(|dt| dt.to_rfc3339()).unwrap_or_default(),
                    daemon: name,
                    level: "error".to_string(),
                    message,
                    fields: None,
                };
                self.write_log(entry).await?;
            }
            DaemonMessage::StatusUpdate { name, status } => {
                let entry = LogEntry {
                    timestamp: DateTime::from_timestamp(
                        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                        0
                    ).map(|dt| dt.to_rfc3339()).unwrap_or_default(),
                    daemon: name,
                    level: "info".to_string(),
                    message: format!("Status changed to {:?}", status),
                    fields: None,
                };
                self.write_log(entry).await?;
            }
            DaemonMessage::Register { name, pid, version } => {
                let entry = LogEntry {
                    timestamp: DateTime::from_timestamp(
                        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                        0
                    ).map(|dt| dt.to_rfc3339()).unwrap_or_default(),
                    daemon: name.clone(),
                    level: "info".to_string(),
                    message: format!("Daemon registered (pid={}, v={})", pid, version),
                    fields: None,
                };
                self.write_log(entry).await?;
            }
            DaemonMessage::Shutdown { name, reason } => {
                let entry = LogEntry {
                    timestamp: DateTime::from_timestamp(
                        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                        0
                    ).map(|dt| dt.to_rfc3339()).unwrap_or_default(),
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("[logd] Osiris Log Daemon v{} starting", VERSION);

    let logd = Logd::new()?;
    logd.open_persist_files().await?;
    logd.connect_to_bridge().await?;
    logd.register().await?;

    // Wait for acknowledgment with timeout
    tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        let mut interval = interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if let Err(e) = logd.handle_bridge_messages().await {
                debug!("[logd] Bridge message handler error: {}", e);
            }
        }
    }).await.ok(); // Ignore timeout, continue if registered

    info!("[logd] Registration complete, entering main loop");

    // Main loop - handle bridge messages and periodic flush
    let mut flush_interval = interval(Duration::from_millis(FLUSH_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = flush_interval.tick() => {
                if let Err(e) = logd.flush().await {
                    error!("[logd] Flush error: {}", e);
                }
            }
            _ = async {
                if let Err(e) = logd.handle_bridge_messages().await {
                    debug!("[logd] Bridge message handler error: {}", e);
                }
            } => {}
        }
    }
}
// Ma'at — message.rs
// The language Bridge and daemons speak to each other.
// Every message over the Unix socket is one of these types.

use serde::{Deserialize, Serialize};
use crate::daemon::DaemonStatus;

/// Messages a daemon sends TO the Bridge
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Daemon has started and is registering itself
    Register {
        name:    String,
        pid:     u32,
        version: String,
    },
    /// Daemon is reporting its current status
    StatusUpdate {
        name:   String,
        status: DaemonStatus,
    },
    /// Daemon encountered an error worth logging
    Error {
        name:    String,
        message: String,
    },
    /// Daemon is shutting down cleanly
    Shutdown {
        name:   String,
        reason: String,
    },
}

/// Messages the Bridge sends TO a daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BridgeMessage {
    /// Bridge acknowledges daemon registration
    Acknowledged {
        name: String,
    },
    /// Bridge requests daemon status
    StatusRequest,
    /// Bridge instructs daemon to stop gracefully
    Stop,
    /// Bridge instructs daemon to reload its config
    Reload,
    /// Bridge instructs daemon to restart
    Restart,
}

/// A framed message for Unix socket transport
/// Serialized as newline-delimited JSON
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub payload: String, // JSON-encoded DaemonMessage or BridgeMessage
}

impl Frame {
    pub fn encode<T: Serialize>(msg: &T) -> Result<String, String> {
        let json = serde_json::to_string(msg)
            .map_err(|e| format!("Frame encode error: {}", e))?;
        Ok(format!("{}\n", json))
    }

    pub fn decode_daemon_message(raw: &str) -> Result<DaemonMessage, String> {
        serde_json::from_str(raw.trim())
            .map_err(|e| format!("Frame decode error: {}", e))
    }

    pub fn decode_bridge_message(raw: &str) -> Result<BridgeMessage, String> {
        serde_json::from_str(raw.trim())
            .map_err(|e| format!("Frame decode error: {}", e))
    }
}
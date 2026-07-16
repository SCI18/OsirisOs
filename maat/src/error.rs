// Ma'at — error.rs
// Shared error types across The Abyss Network

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MaatError {
    #[error("Daemon '{0}' not found in registry")]
    DaemonNotFound(String),

    #[error("Daemon '{0}' failed to register within timeout")]
    RegistrationTimeout(String),

    #[error("Daemon '{0}' dependency '{1}' is not running")]
    DependencyNotMet(String, String),

    #[error("IPC error: {0}")]
    IpcError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Registry error: {0}")]
    RegistryError(String),
}
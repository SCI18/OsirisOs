// Core types describing every daemon in the Abyss Network

use serde::{Deserialize, Serialize};

// The 9 Domains - 

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]

pub enum DaemonDomain {
    SystemCore,
    Hardware,
    Input,
    DisplayGraphics,
    NetworkAkernet,
    Audio,
    UserSession,
    Security,
    Services,
}

impl DaemonDomain {
    pub fn as_str(&self) -> &str {
        match self {
            DaemonDomain::SystemCore        => "system_core",
            DaemonDomain::Hardware          => "hardware",
            DaemonDomain::Input             => "input",
            DaemonDomain::DisplayGraphics   => "display_graphics",
            DaemonDomain::NetworkAkernet    => "network_akernet",
            DaemonDomain::Audio             => "audio",
            DaemonDomain::UserSession       => "user_session",
            DaemonDomain::Security          => "security",
            DaemonDomain::Services          => "services",
        }
    }
}

// The Lifecycle status of the Daemons - 

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]

pub enum DaemonStatus {
    Pending,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
    Restarting,
}

impl DaemonStatus {
    pub fn as_str(&self) -> &str {
        match self {
            DaemonStatus::Pending      => "pending",
            DaemonStatus::Starting     => "starting",
            DaemonStatus::Running      => "running",
            DaemonStatus::Stopping     => "stopping",
            DaemonStatus::Stopped      => "stopped",
            DaemonStatus::Failed       => "failed",
            DaemonStatus::Restarting   => "restarting",
        }
    }
    pub fn is_healthy(&self) -> bool{
        matches!(self, DaemonStatus::Running)
    }
}

/// Bridge tracking for the 42
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub id:            u8,
    pub name:          String,
    pub description:   String,
    pub domain:        DaemonDomain,
    pub status:        DaemonStatus,
    pub pid:           Option<u32>,
    pub depends_on:    Vec<String>,
    pub restartable:   bool,
    pub restart_count: u32,
}

impl DaemonInfo {
    pub fn new(
     id:               u8,
     name:             &str,
     description:      &str,
     domain:           DaemonDomain,
     depends_on:       Vec<&str>,   
    ) -> Self {
        DaemonInfo {
            id,           
            name:             name.to_string(),
            description:      description.to_string(),
            domain,
            status:           DaemonStatus::Pending,
            pid:              None,
            depends_on:       depends_on.iter().map(|s| s.to_string()).collect(),
            restartable:      true,
            restart_count:    0,
        }
    }
    pub fn is_ready_to_spawn(&self, running: &[String]) -> bool {
        self.depends_on.iter().all(|dep| running.contains(dep))
    }
}
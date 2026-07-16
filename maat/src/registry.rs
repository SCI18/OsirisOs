// Ma'at — registry.rs
// The 42 Assessors. The complete roster of The Abyss Network.
// Bridge reads this at boot to know who must exist and in what order.

use crate::daemon::{DaemonInfo, DaemonDomain, DaemonStatus};

pub struct DaemonRegistry {
    pub daemons: Vec<DaemonInfo>,
}

impl DaemonRegistry {
    /// Build the full 42 daemon registry
    /// This is the canonical source of truth for The Abyss Network
    pub fn load() -> Self {
        DaemonRegistry {
            daemons: vec![

                // ── Domain 1 — System Core ─────────────────────────────
                DaemonInfo::new(
                    1, "kha-watchd",
                    "Monitors Kha itself, system heartbeat",
                    DaemonDomain::SystemCore,
                    vec![],
                ),
                DaemonInfo::new(
                    2, "logd",
                    "Unified system logging, ring buffer",
                    DaemonDomain::SystemCore,
                    vec![],  // logd has no dependencies — first to spawn
                ),
                DaemonInfo::new(
                    3, "healthd",
                    "CPU/RAM monitoring, system health reporting",
                    DaemonDomain::SystemCore,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    4, "timed",
                    "System time, NTP sync",
                    DaemonDomain::SystemCore,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    5, "entropyd",
                    "Entropy pool management, RNG seeding",
                    DaemonDomain::SystemCore,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    6, "mountd",
                    "Filesystem mount management post-boot",
                    DaemonDomain::SystemCore,
                    vec!["logd"],
                ),

                // ── Domain 2 — Hardware ────────────────────────────────
                DaemonInfo::new(
                    7, "batteryd",
                    "Charge state, health, thermal limits",
                    DaemonDomain::Hardware,
                    vec!["logd", "healthd"],
                ),
                DaemonInfo::new(
                    8, "modemd",
                    "Telephony, SIM, carrier management",
                    DaemonDomain::Hardware,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    9, "camerad",
                    "Camera pipeline, sensor management",
                    DaemonDomain::Hardware,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    10, "sensorsd",
                    "Gyro, accelerometer, proximity, light",
                    DaemonDomain::Hardware,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    11, "vibrd",
                    "Haptic feedback",
                    DaemonDomain::Hardware,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    12, "gpiod",
                    "Hardware kill switch states, GPIO events",
                    DaemonDomain::Hardware,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    13, "thermald",
                    "Thermal zones and throttling policy",
                    DaemonDomain::Hardware,
                    vec!["logd", "healthd"],
                ),
                DaemonInfo::new(
                    14, "fwatchd",
                    "Firmware/hardware fault monitoring",
                    DaemonDomain::Hardware,
                    vec!["logd", "healthd"],
                ),

                // ── Domain 3 — Input ───────────────────────────────────
                DaemonInfo::new(
                    15, "inputd",
                    "Unified input via libinput — keyboard, mouse, touch, stylus",
                    DaemonDomain::Input,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    16, "btd",
                    "Bluetooth stack, HID pairing, reconnection",
                    DaemonDomain::Input,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    17, "usbd",
                    "USB OTG, hotplug, gadget mode switching",
                    DaemonDomain::Input,
                    vec!["logd"],
                ),

                // ── Domain 4 — Display & Graphics ──────────────────────
                DaemonInfo::new(
                    18, "displayd",
                    "Output management, external display routing, convergence trigger",
                    DaemonDomain::DisplayGraphics,
                    vec!["logd", "inputd"],
                ),
                DaemonInfo::new(
                    19, "compositord",
                    "Ra Wayland compositor",
                    DaemonDomain::DisplayGraphics,
                    vec!["logd", "displayd", "sessiond"],
                ),
                DaemonInfo::new(
                    20, "brightd",
                    "Display brightness, adaptive light response",
                    DaemonDomain::DisplayGraphics,
                    vec!["logd", "displayd", "sensorsd"],
                ),
                DaemonInfo::new(
                    21, "rotationd",
                    "Screen rotation, orientation lock",
                    DaemonDomain::DisplayGraphics,
                    vec!["logd", "displayd", "sensorsd"],
                ),

                // ── Domain 5 — Network / AkerNet ───────────────────────
                DaemonInfo::new(
                    22, "netd",
                    "Core network management, interfaces",
                    DaemonDomain::NetworkAkernet,
                    vec!["logd", "mountd"],
                ),
                DaemonInfo::new(
                    23, "wifid",
                    "WiFi scanning, connection, profiles",
                    DaemonDomain::NetworkAkernet,
                    vec!["logd", "netd"],
                ),
                DaemonInfo::new(
                    24, "dnsd",
                    "DNS filtering, AdGuardHome integration",
                    DaemonDomain::NetworkAkernet,
                    vec!["logd", "netd"],
                ),
                DaemonInfo::new(
                    25, "vpnd",
                    "VPN lifecycle, kill switch",
                    DaemonDomain::NetworkAkernet,
                    vec!["logd", "netd", "dnsd"],
                ),
                DaemonInfo::new(
                    26, "firewalld",
                    "Packet filtering, per-app rules",
                    DaemonDomain::NetworkAkernet,
                    vec!["logd", "netd"],
                ),
                DaemonInfo::new(
                    27, "proxyd",
                    "Traffic routing, privacy proxy",
                    DaemonDomain::NetworkAkernet,
                    vec!["logd", "netd", "dnsd"],
                ),

                // ── Domain 6 — Audio ───────────────────────────────────
                DaemonInfo::new(
                    28, "audiod",
                    "PipeWire core, audio routing",
                    DaemonDomain::Audio,
                    vec!["logd", "sessiond"],
                ),
                DaemonInfo::new(
                    29, "voiced",
                    "Call audio, noise cancellation, mic routing",
                    DaemonDomain::Audio,
                    vec!["logd", "audiod", "modemd"],
                ),
                DaemonInfo::new(
                    30, "meliad",
                    "Media session management, playback controls",
                    DaemonDomain::Audio,
                    vec!["logd", "audiod"],
                ),

                // ── Domain 7 — User Session ────────────────────────────
                DaemonInfo::new(
                    31, "sessiond",
                    "User session lifecycle, login state",
                    DaemonDomain::UserSession,
                    vec!["logd", "cryptd"],
                ),
                DaemonInfo::new(
                    32, "appd",
                    "App launch, sandbox management",
                    DaemonDomain::UserSession,
                    vec!["logd", "sessiond", "permd"],
                ),
                DaemonInfo::new(
                    33, "convergenced",
                    "DeX-style mode switching, layout management",
                    DaemonDomain::UserSession,
                    vec!["logd", "displayd", "inputd", "sessiond"],
                ),
                DaemonInfo::new(
                    34, "clipboardd",
                    "Clipboard management across convergence modes",
                    DaemonDomain::UserSession,
                    vec!["logd", "sessiond"],
                ),

                // ── Domain 8 — Security ────────────────────────────────
                DaemonInfo::new(
                    35, "cryptd",
                    "Disk encryption, key management",
                    DaemonDomain::Security,
                    vec!["logd", "mountd"],
                ),
                DaemonInfo::new(
                    36, "permd",
                    "App permission enforcement",
                    DaemonDomain::Security,
                    vec!["logd", "auditd"],
                ),
                DaemonInfo::new(
                    37, "auditd",
                    "Security event logging, anomaly detection",
                    DaemonDomain::Security,
                    vec!["logd"],
                ),
                DaemonInfo::new(
                    38, "biod",
                    "Biometric authentication, fingerprint",
                    DaemonDomain::Security,
                    vec!["logd", "sessiond"],
                ),

                // ── Domain 9 — Services ────────────────────────────────
                DaemonInfo::new(
                    39, "notifyd",
                    "Notification routing, priority management",
                    DaemonDomain::Services,
                    vec!["logd", "sessiond"],
                ),
                DaemonInfo::new(
                    40, "syncd",
                    "Background sync, app data management",
                    DaemonDomain::Services,
                    vec!["logd", "netd", "sessiond"],
                ),
                DaemonInfo::new(
                    41, "locationd",
                    "GPS, network location, privacy controls",
                    DaemonDomain::Services,
                    vec!["logd", "netd", "permd"],
                ),
                DaemonInfo::new(
                    42, "updated",
                    "OPIUM update checks, delta patching",
                    DaemonDomain::Services,
                    vec!["logd", "netd", "opium"],
                ),
            ],
        }
    }

    /// Get a daemon by name
    pub fn get(&self, name: &str) -> Option<&DaemonInfo> {
        self.daemons.iter().find(|d| d.name == name)
    }

    /// Get all daemons in a domain
    pub fn by_domain(&self, domain: &DaemonDomain) -> Vec<&DaemonInfo> {
        self.daemons.iter().filter(|d| &d.domain == domain).collect()
    }

    /// Get daemons that are ready to spawn given currently running daemons
    pub fn ready_to_spawn(&self, running: &[String]) -> Vec<&DaemonInfo> {
        self.daemons
            .iter()
            .filter(|d| {
                d.status == DaemonStatus::Pending
                    && d.is_ready_to_spawn(running)
            })
            .collect()
    }

    /// Get names of all running daemons
    pub fn running_names(&self) -> Vec<String> {
        self.daemons
            .iter()
            .filter(|d| d.status == DaemonStatus::Running)
            .map(|d| d.name.clone())
            .collect()
    }

    /// System health — true if all restartable daemons are Running
    pub fn is_healthy(&self) -> bool {
        self.daemons
            .iter()
            .filter(|d| d.restartable)
            .all(|d| d.status.is_healthy())
    }

    /// Count daemons by status
    pub fn status_summary(&self) -> String {
        let running  = self.daemons.iter().filter(|d| d.status == DaemonStatus::Running).count();
        let pending  = self.daemons.iter().filter(|d| d.status == DaemonStatus::Pending).count();
        let failed   = self.daemons.iter().filter(|d| d.status == DaemonStatus::Failed).count();
        let stopped  = self.daemons.iter().filter(|d| d.status == DaemonStatus::Stopped).count();
        format!(
            "running={} pending={} failed={} stopped={}",
            running, pending, failed, stopped
        )
    }
}
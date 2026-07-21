# Networks Spec — NetworkAkernet Domain Build Log

## 2026-07-21 — NetworkAkernet Domain (Daemons #22-27) Built

### Summary
Successfully created and built all 6 daemons in the NetworkAkernet domain (Domain 5 of The Abyss Network) for the Termux/proot target architecture (aarch64).

### Daemons Created

| ID | Name | Responsibility | Path |
|----|------|----------------|------|
| 22 | **netd** | Core network management, interfaces | `daemons/network_akernet/netd/` |
| 23 | **wifid** | WiFi scanning, connection, profiles | `daemons/network_akernet/wifid/` |
| 24 | **dnsd** | DNS filtering, AdGuardHome integration | `daemons/network_akernet/dnsd/` |
| 25 | **vpnd** | VPN lifecycle, kill switch | `daemons/network_akernet/vpnd/` |
| 26 | **firewalld** | Packet filtering, per-app rules | `daemons/network_akernet/firewalld/` |
| 27 | **proxyd** | Traffic routing, privacy proxy | `daemons/network_akernet/proxyd/` |

### Implementation Details

#### Common Architecture (all 6 daemons)
- **Ma'at Registration**: Full DaemonMessage::Register / BridgeMessage::Acknowledged handshake
- **Unix Socket IPC**: Connects to AkerNet Bridge at `/tmp/osiris-bridge.sock`
- **Persistent Reader Task**: Spawned background task owns socket read half (avoids select! race from early design)
- **Status Updates**: Periodic `DaemonMessage::StatusUpdate { status: DaemonStatus::Running }`
- **Graceful Shutdown**: Handles `BridgeMessage::Stop` → emits `DaemonMessage::Shutdown` → exits cleanly
- **Reload/Restart**: Handles `BridgeMessage::Reload` / `Restart` per spec
- **Alert Emission**: `emit_alert()` with 60s deduplication + severity escalation (Info→Warning→Critical)
- **Polling Loop**: `tokio::select!` with poll interval + Bridge message receiver

#### Protocol Compliance
- Uses existing `DaemonMessage::Alert` variant with `AlertSeverity` enum (approved in networks-spec-log.md 2026-07-16)
- Uses existing `BridgeMessage::Forward` variant for logd forwarding (approved in nsp.md)
- All message types: `Register`, `StatusUpdate`, `Error`, `Shutdown`, `Alert`
- All bridge messages: `Acknowledged`, `RegistrationRejected`, `StatusRequest`, `Stop`, `Reload`, `Restart`, `Forward`

#### Termux/proot Considerations
- No elevated privileges required (uses standard `/proc/net`, `/sys/class/net` for monitoring)
- No external binary dependencies in scaffolding (nl80211, iptables, nftables, sing-box integration deferred to implementation phase)
- Poll intervals conservative (10-30s) for battery/CPU on mobile
- All file I/O via `tokio::fs` (async) — no blocking std::fs

### Files Created

```
daemons/network_akernet/
├── netd/
│   ├── Cargo.toml
│   └── src/main.rs
├── wifid/
│   ├── Cargo.toml
│   └── src/main.rs
├── dnsd/
│   ├── Cargo.toml
│   └── src/main.rs
├── vpnd/
│   ├── Cargo.toml
│   └── src/main.rs
├── firewalld/
│   ├── Cargo.toml
│   └── src/main.rs
└── proxyd/
    ├── Cargo.toml
    └── src/main.rs
```

### Workspace Integration
- Added all 6 daemons to root `Cargo.toml` workspace members (lines 21-26)
- All use `maat = { path = "../../../maat" }` for protocol types
- All use workspace dependencies: `tokio`, `serde`, `anyhow`, `tracing`, `chrono`

### Build Verification
```bash
$ cargo build --workspace
# → Finished `dev` profile [unoptimized + debuginfo] target(s) in 19.14s
```

**Result**: ✅ Clean build — only pre-existing warnings (unused imports, dead_code in opium/harvester/kha/mountd), no new errors introduced.

### Dependencies on Other Specs (Noted for Cross-Domain Coordination)

| Dependency | Owner | Status |
|------------|-------|--------|
| `netd` depends on `mountd` (per registry) | Daemons Spec | `mountd` exists in SystemCore |
| `netd`, `wifid`, `dnsd`, `vpnd`, `firewalld`, `proxyd` all depend on `logd` | Daemons Spec | `logd` exists in SystemCore |
| `dnsd` → AdGuardHome API integration | Daemons Spec | Stubbed; implementation deferred |
| `vpnd`/`firewalld`/`proxyd` → kernel netlink/iptables/nftables | Daemons Spec | Requires CAP_NET_ADMIN; implementation deferred |
| `wifid` → nl80211 / iwd / wpa_supplicant | Daemons Spec | Stubbed; implementation deferred |

### Next Steps (Implementation Phase)
1. **netd**: Implement `/proc/net/dev` parsing, netlink monitoring for carrier/IP changes
2. **wifid**: Integrate `nl80211` via `neli` or `rtnetlink` crate for scan/connect
3. **dnsd**: AdGuardHome REST API client for stats/blocklist management
4. **vpnd**: WireGuard/OpenVPN process management + nftables kill switch
5. **firewalld**: nftables rule management with per-app labels (requires kernel 5.10+)
6. **proxyd**: sing-box / v2ray-core integration with profile rotation

### Networks Spec Decisions Logged
No new protocol/enum changes required — all 6 daemons conform to existing Ma'at contracts established for SystemCore daemons.

---
**Status**: Domain 5 (NetworkAkernet) scaffolding complete. Ready for Daemons Spec to implement monitoring logic.
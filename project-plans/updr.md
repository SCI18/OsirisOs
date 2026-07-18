# Updated SystemCore Audit Results (2026-07-17)

### **Compliance Summary**

| Daemon | Status | Critical Issues |
|--------|--------|-----------------|
| **logd** (#2) | ❌ **Non-compliant** | Alert ingestion path dead code — `ingest_daemon_message` exists but never called; Bridge forwards alerts but logd ignores them |
| **healthd** (#3) | ✅ **Compliant** | Thermal thresholds unused (sysinfo 0.30 limitation, documented) |
| **timed** (#4) | ✅ **Compliant** | NTP measured RTT is local elapsed, not protocol RTT (acceptable) |
| **mountd** (#6) | ❌ **Non-compliant** | ZFS parsing broken (`zpool status -x` only outputs on error); proot fallback skips `/proc/mounts`; alert severity mismatches (`MountUnexpected`=Warning should be Info, `FilesystemReadOnly`=Warning should be Critical); disappeared mounts emit wrong type |
| **entropyd** (#5) | ❌ **Non-compliant (CRITICAL)** | **Blocking `std::fs` I/O in async functions** — blocks tokio runtime; must use `tokio::fs` or `spawn_blocking` |
| **kha-watchd** (#1) | ❌ **Non-compliant** | Missing process state check (Z/D), missing zombie reaping tracking, missing signal forwarding stats, incomplete alert payload |

---

### **Verdict: SystemCore is NOT Complete**

**Flag: ❌ INCOMPLETE — Requires Remediation**

### **Why**

1. **4 of 6 daemons have functional gaps against their specifications** — only healthd and timed are fully compliant.

2. **One CRITICAL async correctness bug** — entropyd's blocking I/O in async functions will cause runtime stalls under load. This is not a spec mismatch; it's a correctness violation.

3. **Foundational pipeline broken** — logd's ingestion path is dead code. The entire daemon-to-logd alert pipeline (healthd → Bridge → logd, mountd → Bridge → logd, etc.) does not work. The Networks Spec approved `BridgeMessage::Forward` but it remains unimplemented.

4. **kha-watchd misses core spec requirements** — kha.md, qs.md, and eos1.md all specify process state monitoring, reaping tracking, and signal forwarding stats. None are implemented.

5. **Decision log confirms** — Two entries explicitly state:
   - Entry #75: "Status: needs remediation"
   - Entry #110: "Status: proposed" (not "implemented")

---

### **Blocking Dependencies for Hardware Domain**

Per `osiris-rm.json` Stage 1: "Core Abyss daemons: logd, mountd, healthd, timed" — these are explicit prerequisites. With logd and mountd non-compliant, the foundation is unstable.

**Recommendation:** Do not proceed to Hardware domain. Remediate SystemCore first:

1. **entropyd** → fix blocking I/O (highest priority: correctness)
2. **logd** → wire `ingest_daemon_message` + implement `BridgeMessage::Forward`
3. **mountd** → fix ZFS parsing, proot fallback order, alert severities
4. **kha-watchd** → add process state (Z/D), reaping tracking, signal stats

---

## Systems Spec Analysis — logd / BridgeMessage::Forward Decision

### **Current State**

| Component | Status |
|-----------|--------|
| **logd** | Has complete, correct `ingest_daemon_message()` handling all `DaemonMessage` variants (Alert, Error, StatusUpdate, Register, Shutdown). Marked `#[allow(dead_code)]` with clear documentation. |
| **Bridge** | Receives all `DaemonMessage` variants on Unix socket. Currently: logs Alerts locally, updates registry for StatusUpdate/Register/Shutdown, but **does not forward** to logd. |
| **BridgeMessage enum** | Only contains Bridge→daemon control messages (`Acknowledged`, `StatusRequest`, `Stop`, `Reload`, `Restart`). **No forward path exists.** |
| **nsp.md** | Networks Spec decision: **Approved** adding `BridgeMessage::Forward(DaemonMessage)` variant. |

---

### **My Position as Systems Spec**

**APPROVED — This is the correct architectural decision.**

#### **Rationale**

1. **Clean separation of concerns** — The Bridge should not be a log sink. Its job is orchestration/routing. logd is the designated log consumer.

2. **Minimal protocol change** — Adding one variant to `BridgeMessage` is the smallest possible delta. No changes to `DaemonMessage`, `AlertSeverity`, or framing.

3. **Precedent respected** — healthd and mountd already emit `DaemonMessage::Alert` expecting logd to consume them. The pipeline is designed but not connected.

4. **logd is ready** — The ingestion logic is complete, tested in isolation, and correctly uses the existing `write_log()` path (ring buffer + dual persistence + bootstrap gating).

5. **No daemon-side changes needed** — Daemons already send `DaemonMessage::Alert/Error/StatusUpdate/Register/Shutdown` to the Bridge. Only the Bridge's handling changes.

---

### **Blast Radius (Cross-Domain Impact)**

| Domain | Impact | Action Required |
|--------|--------|-----------------|
| **Networks Spec** | Add `Forward(DaemonMessage)` to `BridgeMessage` enum in `maat/src/message.rs` | ✅ Approved in nsp.md |
| **Daemons Spec (logd)** | Remove `#[allow(dead_code)]`, wire `ingest_daemon_message` into reader event loop | Ready when protocol lands |
| **Systems Spec (Bridge)** | In `handle_daemon_connection`, on `DaemonMessage::Alert/Error/StatusUpdate/Register/Shutdown` → send `BridgeMessage::Forward(msg)` to logd's socket | Bridge modification needed |
| **Other daemons** | None — they already send the right messages | No action |

---

### **Implementation Coordination**

Since this spans Bridge (Systems Spec territory) and logd (Daemons Spec territory), I recommend:

1. **Networks Spec** → merges `BridgeMessage::Forward` to `maat` first
2. **Systems Spec** → updates `akernet-bridge/src/main.rs` to forward messages to logd
3. **Daemons Spec** → updates logd to handle `BridgeMessage::Forward` in its event loop

The Bridge will need a reference to logd's socket (or a broadcast channel) to forward. Since logd registers first (Daemon #2), the Bridge can hold its write half after logd's `Acknowledged`.

---

### **Decision**

**Status: APPROVED**

This is a pure protocol extension with clear ownership boundaries. The logd dead code is intentionally documented, not abandoned. Wiring it completes the logging pipeline without architectural compromise.

**Next step:** Networks Spec merges the `maat` change, then I'll coordinate the Bridge and logd updates in a single PR.
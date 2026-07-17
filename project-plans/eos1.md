# Daemons-Spec Plan: kha-watchd (Assessor #1, SystemCore)

## Scope (from kha.md + qs.md)
- Monitor Kha (PID 1) liveness via `signal(0)` or `/proc/1/stat`
- Emit periodic `StatusUpdate` heartbeat to AkerNet Bridge
- Collect Kha metrics: uptime, zombie reaping count, signal forwarding stats
- Emit `Alert` (Critical) on anomalies: missing heartbeat, stopped reaping
- **No mount monitoring** (that's mountd per qs.md)

---

## Implementation Steps

1. **Create crate** at `daemons/system_core/kha-watchd/` with `Cargo.toml` (deps: maat, tokio, anyhow, serde, tracing, libc)

2. **Main loop** (same pattern as healthd/timed/mountd):
   - Register with Ma'at → connect to AkerNet Bridge (`/tmp/osiris-bridge.sock`)
   - Poll interval: 10s (faster than others — heartbeat daemon)
   - `check_kha()` function:
     - `kill(Pid::from_raw(1), Signal::None)` — liveness check
     - Parse `/proc/1/stat` for uptime, state, children
     - Track zombie reaping delta since last poll
   - `emit_alert()` with deduplication (copy healthd pattern)

3. **Alert payload schema**:
   ```json
   { "metric": "kha_liveness", "pid": 1, "alive": false, "uptime_secs": 1234, "zombies_reaped": 0 }
   ```

4. **Add to workspace** in root `Cargo.toml`

5. **Build & verify** `cargo build -p kha-watchd`

---

## Dependencies
- Networks Spec: `DaemonMessage::Alert` + `AlertSeverity` already approved (healthd/mountd precedent) — no new protocol work needed
- Systems Spec: Kha must exist and spawn AkerNet Bridge first (kha-watchd starts last in SystemCore)

---

## Open Question
**Threshold for "missing heartbeat" alert?** Kha is PID 1 — it shouldn't miss heartbeats. Suggest: alert if `signal(0)` fails (process dead) OR `/proc/1/stat` unreadable. No time-based threshold needed — binary liveness check.

---

Ready to implement.
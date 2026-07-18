ingest_daemon_message is left as correct, working code, but explicitly marked #[allow(dead_code)] with a comment explaining exactly why it's unreachable — there's no protocol path for Bridge to forward other daemons' DaemonMessage::Alert frames to logd yet. Wiring it up for real needs a BridgeMessage::Forward(DaemonMessage) variant or similar, which is a Networks Spec decision, not something to invent silently inside a logd fix.

---

## Networks Spec Decision: BridgeMessage::Forward variant

**Decision:** Approve adding `BridgeMessage::Forward(DaemonMessage)` variant.

**Rationale:**
- logd is the designated consumer of all daemon alerts/errors/status — it needs this path
- The Bridge already receives these messages on its socket from daemons (see akernet-bridge `handle_daemon_connection`)
- Currently the Bridge only logs Alerts locally (`tracing::warn!`) but doesn't forward them
- This is a pure protocol extension — no changes to existing `DaemonMessage` or `AlertSeverity` enums
- Precedent: healthd and mountd already emit `DaemonMessage::Alert` expecting logd to consume them

**Affects:**
- `maat/src/message.rs` — add `Forward(DaemonMessage)` to `BridgeMessage`
- `akernet/akernet-bridge/src/main.rs` — in `handle_daemon_connection`, on receiving `DaemonMessage::Alert/Error/StatusUpdate/Register/Shutdown`, forward to logd via new variant
- `daemons/system_core/logd/src/main.rs` — remove `#[allow(dead_code)]`, wire `ingest_daemon_message` into the reader event loop

**Status:** Approved — ready for Daemons Spec to implement in logd and Systems Spec to coordinate Bridge changes.
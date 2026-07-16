# networks-spec — Decision Log

_Newest entries at the bottom._

## 2026-07-16 — mountd requires Alert variant and AlertSeverity enum
Decision: Approve `DaemonMessage::Alert` variant and `AlertSeverity` enum (already added for healthd precedent) for mountd's mount monitoring alerts
Rationale: mountd (Daemon #6, SystemCore) requires `DaemonMessage::Alert` with `AlertSeverity` enum to emit mount monitoring alerts (MountMissing, MountDegraded, MountUnexpected, MountPropagationChanged, FilesystemFull, FilesystemReadOnly, ZfsPoolDegraded, BtrfsScrubRunning). The Networks Spec log confirms healthd precedent established this enum variant.
Affects: mountd (Daemon #6, SystemCore), healthd (precedent), logd (consumes alerts), Ma'at registry (routes Alert messages)
Status: approved

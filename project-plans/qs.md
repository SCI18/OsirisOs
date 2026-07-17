Two follow-ups on the kha-watchd proposal:
Kha's boot mounts: Kha directly mounts /proc, /sys, /dev, /run at boot as part of its own init responsibility — separate from mountd's fstab-based monitoring. If one of these Kha-mounted filesystems disappears or gets remounted unexpectedly, whose job is detecting that — kha-watchd's (since Kha set it up) or mountd's (since it's still a mount)? Justify the answer, don't just assert it.
fwatchd claim: You referenced fwatchd (Daemon #14, Hardware domain, firmware/hardware fault monitoring) as a clarifying aside. Where did that come from — is it explicitly listed in osiris-rm.json or another file in the repo, or was that recalled from general knowledge of what a Hardware-domain daemon set might look like? Show the source.

---

### Answers

#### 1. Kha's boot mounts — detection responsibility: **mountd**, not kha-watchd

**Justification:**

- **Kha's role** (per `osiris-rm.json`): "Mounts essential filesystems, reaps zombies, forwards signals. Single responsibility: stay alive and hand control to AkerNet Bridge." Kha mounts `/proc`, `/sys`, `/dev`, `/run` **once at boot** as part of its PID 1 duty.
- **mountd's role** (per `osiris-rm.json` + `mountd.md`): "Filesystem mount management **post-boot**." Monitors `/proc/self/mountinfo` + `/etc/fstab`. Detects missing mounts, fs-type mismatches, unexpected mounts, propagation changes, disk usage.
- **kha-watchd's role** (per `osiris-rm.json`): "Monitors **Kha itself**, system heartbeat." Scope: Kha liveness (PID 1), child reaping, signal forwarding, heartbeat emission.

**Architectural separation:**
- Kha → **sets up** the mount namespace
- mountd → **monitors** the mount namespace continuously (post-boot)
- kha-watchd → monitors **Kha the process**, not the filesystems Kha mounted

If `/proc` disappears, it's a **mount namespace anomaly** → mountd detects via `/proc/self/mountinfo` diff and emits `MountMissing`/`MountUnexpected`. kha-watchd would only care if Kha (PID 1) stops responding — a different failure mode.

**Blast radius:** This is correct separation of concerns. No overlap. mountd owns mount monitoring; kha-watchd owns Kha process monitoring.

---

#### 2. fwatchd source — **explicitly listed in osiris-rm.json**

**Source:** `osiris-rm.json` lines 134-138 (under `the_abyss_network.domains.hardware.daemons`):

```json
{
  "id": 14,
  "name": "fwatchd",
  "responsibility": "Firmware/hardware fault monitoring"
}
```

It is Daemon #14 of 42, in the **Hardware domain** (domain id: 2, daemon_count: 8). Not recalled from general knowledge — directly from the project's own roadmap document. 


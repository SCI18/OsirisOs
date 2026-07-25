# sec-audit-1.md — First Security Spec Audit Request

**Date:** 2026-07-21
**Scope:** Everything completed to date — first standing audit per new Security Spec mandate

## Instructions
Read every file listed below in full before writing any finding. Quote code verbatim for every claim, with exact file/line/function. Do not rely on any other agent's decision log or status report as ground truth — verify against the actual current source. If a listed file isn't available to you, say so explicitly rather than reasoning around the gap.

## Scope

### 1. Core orchestration
- `maat/src/*.rs` (message.rs, daemon.rs, registry.rs, error.rs, lib.rs)
- `akernet/akernet-bridge/src/main.rs`
- `kha/src/main.rs`

### 2. SystemCore daemons (6)
- `daemons/system_core/{logd,healthd,timed,mountd,entropyd,kha-watchd}/src/main.rs`

### 3. NetworkAkernet daemons (6)
- `daemons/network_akernet/{netd,wifid,dnsd,vpnd,firewalld,proxyd}/src/main.rs`
- Specifically check: does `wifid`'s `iw dev scan` subprocess call block the tokio runtime (no `spawn_blocking`)? This was flagged as unverified by Sitratis directly.

### 4. Harvester (full package-management stack)
- `harvester/src/{main,config,harvest,install,remove,manifest}.rs`
- Specifically verify the 2026-07-17 remediation actually holds under adversarial input, not just the happy path — e.g. can `validate_package_name` be bypassed via encoding tricks, does the checksum verification actually run before trust is extended anywhere, does the atomic-install temp-dir cleanup leave a race window.

## What to focus on
- Anything reachable from untrusted input (a package name from CLI args, a `.osr` file from an untrusted source, a socket connection from any local process)
- Privilege boundaries — what runs as root vs not, and whether that's necessary
- Anything that shells out to an external binary — injection risk, blocking-call risk, or trusting that binary's output uncritically
- Script execution (Harvester's Scripts struct) — already flagged as unsandboxed; confirm nothing else compounds that risk (e.g. scripts running with more privilege than the installing user)

## Output format
Same structure as the Harvester audit precedent (dict3.md): executive summary table (issue / file:line / severity / type), then verified findings with verbatim code quotes, then a prioritized fix list. Write to `docs/decisions/sec-audit-1-results.md`.

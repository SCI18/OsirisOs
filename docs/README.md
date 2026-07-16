# Osiris OS Agent Team — Manual Setup (Phase 1)

This is the **manual-routing** version: 4 role instruction files you paste into Aider or OpenCode sessions yourself, with you (Sitratis) acting as the router between them. Once you've got a feel for this, we automate it (Phase 2 — not in this doc).

---

## 1. Environment setup (inside proot Debian)

You're working in Termux + proot Debian. All commands below run **inside proot Debian**, not raw Termux, so they sit next to your Osiris OS repo and your existing git/SSH setup.

### 1a. Node.js (needed for OpenCode; Aider is Python-only)
You already prefer nvm over direct installs — same here:
```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
source ~/.bashrc
nvm install 22
nvm use 22
node --version   # confirm v22.x.x
```

### 1b. Ollama (the inference client — routes to Ollama Cloud, no local model needed)
Use the standard Linux install script inside proot Debian:
```bash
curl -fsSL https://ollama.com/install.sh | sh
ollama --version   # confirm install
```
> If this fails silently inside proot (some curl-piped installers mis-detect containerized environments), fall back to the Termux-native package as a sanity check: exit to bare Termux and run `pkg install ollama`. But keep your actual working install inside proot Debian so it's on the same filesystem as your repo.

### 1c. Ollama Cloud account + API key
1. Sign up / log in at https://ollama.com
2. Go to your account settings, generate an API key
3. Export it in proot Debian (add to `~/.bashrc` so it persists):
```bash
export OLLAMA_API_KEY=<your-key-here>
echo 'export OLLAMA_API_KEY=<your-key-here>' >> ~/.bashrc
```

### 1d. Aider (Python-based, git-native — good for Systems Spec / Networks Spec, propose-only roles)
```bash
python3 -m venv ~/.aider-env
source ~/.aider-env/bin/activate
pip install aider-chat
aider --version   # confirm install
```

### 1e. OpenCode (Node-based, full autonomy incl. shell commands — good for Daemons Spec / GUI-UX Spec, full-write roles)
```bash
npm install -g opencode-ai   # verify exact package name against https://opencode.ai docs before running — naming has shifted between forks
opencode --version
```
> Note: there are multiple "OpenCode"-named projects in this space. Confirm you're installing the one that supports custom OpenAI-compatible endpoints (Ollama) before relying on it — check `opencode --help` for a `--model` or provider-config flag once installed.

### 1e (Termux/proot detail worth checking upfront)
Android 15's Phantom Process Killer can silently kill long-running background processes (this already bit your XFCE4 sessions). Since both Aider and OpenCode may run long sessions, make sure you've already applied the earlier fix:
- Developer Options → "Disable child process restrictions"
- Termux wake lock enabled (`termux-wake-lock` command, or the notification toggle)

---

## 2. Repo setup

Copy the role files and decision logs into your Osiris OS repo:
```bash
cp -r roles/ /path/to/osiris-os-repo/docs/agent-roles/
cp -r docs/decisions/ /path/to/osiris-os-repo/docs/decisions/
cd /path/to/osiris-os-repo
git add docs/agent-roles docs/decisions
git commit -m "Add agent team role specs and decision logs"
```

---

## 3. Running a manual session

### Aider (propose-only roles: Systems Spec, Networks Spec)
```bash
cd /path/to/osiris-os-repo
source ~/.aider-env/bin/activate
aider --model ollama_chat/glm-5.1:cloud
```
Once inside, paste the contents of `docs/agent-roles/systems-spec.md` (or networks-spec.md) as your first message, then give your actual task. Since these roles are propose-only, tell Aider explicitly not to auto-commit:
```bash
aider --model ollama_chat/glm-5.1:cloud --no-auto-commits
```
Review every diff before accepting. If Aider can't apply an edit cleanly, that's usually a sign the model needs a more specific instruction, not that something's broken.

### OpenCode (full-write roles: Daemons Spec, GUI/UX Spec)
```bash
cd /path/to/osiris-os-repo
opencode
```
Paste `docs/agent-roles/daemons-spec.md` (or gui-ux-spec.md) as your first message. Confirm it's pointed at Ollama Cloud (check its config file, likely `~/.config/opencode/opencode.json`, for the model/provider settings) before running anything that touches real files.

---

## 4. The manual routing workflow

There is no "team channel" — you are the router. Practical pattern:

1. Start a session in the role you need (e.g. Daemons Spec via OpenCode)
2. Give it a task: *"Build the mountd daemon following the healthd template"*
3. If it flags a cross-domain need (e.g. "this needs a new DaemonMessage variant — check with Networks Spec"), **you** open a second terminal/session, load Networks Spec's instructions, and relay the question
4. Bring the answer back to the first session
5. At the end of each session, confirm the model actually appended to its decision log — don't assume it did; check the file yourself with `cat docs/decisions/daemons-spec-log.md`

Tip: keep 2 terminal tabs open (Termux supports multiple sessions) — one for the "asking" role, one for the "answering" role — so you're not constantly restarting sessions.

---

## 5. Model picks for Osiris OS work

Cloud models on Ollama worth trying, in rough order of coding/agentic strength (verify current names at https://ollama.com/search?c=cloud since the catalog shifts):
- `glm-5.1:cloud` — strong agentic engineering / coding focus
- `qwen3-coder:480b-cloud` — large coder-tuned MoE model
- `minimax-m2.7:cloud` — coding + agentic workflows
- `kimi-k2.6:cloud` — long-horizon coding/agentic, very large

Not every model handles tool-calling/file-edit workflows well — if a model returns correct-looking code but Aider/OpenCode can't parse or apply it, that's a model-capability issue, switch models rather than fighting the tool.

---

## 6. Free vs Pro tier reality check

Free tier: ~5M tokens/week, restricted to lighter models (no glm-5.1 or kimi-k2.6 class). Expect roughly 2-4 daemon-sized sessions per week if each involves real iteration. If you hit walls fast, Pro is $20/mo for 50x the usage and the full model catalog — same price as Claude Pro, but no per-request multiplier tax and no local RAM ceiling since it's cloud-side inference.

---

## 7. What's NOT covered here (Phase 2 — later)
- Automating cross-agent routing (so you don't manually relay messages)
- A shared "standup" file all 4 roles append a status line to
- Stub starter task for Daemons Spec's first real session (mountd)

Flag when you're ready and we'll build these on top of what you've tested manually.

# knock

[한국어](README.md) · **English**

A desktop approval / annotation / question gate for AI coding agents.
Not a browser tab — a **native Tauri window** + OS notification + Dock bounce + keyboard nav.

- Single binary (~9.5MB), Bun/Tauri build
- Simple `stdout` contract → drops into existing skills, hooks, and CLIs
- Replaces blocking chat questions (AskUserQuestion) with a desktop window

## Quick Start (macOS · Apple Silicon)

**1. Install the CLI** — pick one

```bash
brew install hihenen/tap/knock                                                       # recommended (no warning)
curl -fsSL https://raw.githubusercontent.com/hihenen/knock/master/install.sh | bash  # or (without Homebrew)
```

**2. Claude Code plugin** (skills + hook)

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
/reload-plugins
```

**3. Resident daemon** (required)

```bash
knock daemon install   # run at login → menubar tray always visible + zero first-call latency
```

Multiple sessions can call at once — they queue into a single window instead of stacking, and the menubar tray always shows the pending count (badge). Turn on Touch ID right there via the 🔒 toggle in the approval window header.

**4. Agent automation** — plan approval is automatic via the hook, but to route **every other approval / question / web action** through knock, add this to your project (or global) `CLAUDE.md`:

```markdown
## knock — desktop approval/question gate
- When you need **approval** from the user, raise a `knock annotate <md> --gate --json`
  gate instead of asking in chat.
- For **multiple-choice questions**, use `knock ask <json>` instead of AskUserQuestion.
  Put the rationale (background, comparison tables, conclusion) in the top-level
  `context` (markdown) so the user sees the basis for the decision in the window.
- When the user must **click/approve on the web** (Scalr Apply / GitHub PR / ArgoCD /
  dashboard, etc.), pass `--action-url <URL>` so one approval jumps the browser
  straight to that action.
- For critical approvals (prd / IAM / destructive), add `--touch-id` (if enabled via knock settings).
- knock output: annotate=`{"decision":"approved|annotated|dismissed"}`,
  ask=`{"answers":{"<h>":["..."]}}` (always arrays).
```

**That's the whole flow** — install → plugin → resident daemon → agent guidance. Now any session can approve/ask through knock, and concurrent sessions queue into one window.

> **Updates**: when a new version ships, knock shows a banner at the top of the window. `brew upgrade hihenen/tap/knock` (+ `/plugin marketplace update knock` → `/reload-plugins`)

---

## Install (detailed)

> Two tracks. Just copy-paste **your OS section only**, top to bottom. (macOS / Windows kept fully separate so they never mix.)

---

### 🍎 macOS (Apple Silicon) — start to finish

**Step 1 · Install the CLI** (one of three — Homebrew recommended)

```bash
# (recommended) Homebrew — handles Gatekeeper quarantine automatically, no warning
brew install hihenen/tap/knock
```

```bash
# (alternative) without Homebrew — installs to ~/.local/bin/knock + removes quarantine
curl -fsSL https://raw.githubusercontent.com/hihenen/knock/master/install.sh | bash
```

```bash
# (alternative) download the binary directly
curl -L https://github.com/hihenen/knock/releases/latest/download/knock-macos-aarch64 -o knock
chmod +x knock
xattr -c knock                 # remove Gatekeeper quarantine (needed for downloaded builds)
mv knock ~/.local/bin/
```

```bash
# verify
knock --version
```

**Step 2 · Claude Code plugin** (skills + automatic plan-approval hook) — inside Claude Code:

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
/reload-plugins
```

**Step 3 · Resident daemon** (required — runs at login / menubar tray / zero first-call latency)

```bash
knock daemon install
knock daemon status            # confirm
```

**Step 4 · Agent guidance** — paste the snippet from [Quick Start step 4](#quick-start-macos--apple-silicon) into your project (or global `~/.claude/CLAUDE.md`) `CLAUDE.md`. Done.

---

### 🪟 Windows (x64) — start to finish

**Step 1 · Install the CLI** (PowerShell — one of two)

```powershell
# (recommended) install.ps1 — installs knock.exe to %LOCALAPPDATA%\knock + adds to user PATH
irm https://raw.githubusercontent.com/hihenen/knock/master/install.ps1 | iex
```

```powershell
# (alternative) binary directly — grab knock-windows-x64.exe from releases and put it on PATH
#   https://github.com/hihenen/knock/releases/latest  →  knock-windows-x64.exe
```

```powershell
# verify (in a NEW PowerShell window — so PATH is refreshed)
knock --version
```

**Step 2 · Claude Code plugin** (skills + automatic plan-approval hook) — inside Claude Code:

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
/reload-plugins
```

**Step 3 · Resident daemon** (required — runs at login / taskbar resident / zero first-call latency)

```powershell
knock daemon install
knock daemon status            # confirm (registry Run key)
```

**Step 4 · Agent guidance** — paste the snippet from [Quick Start step 4](#quick-start-macos--apple-silicon) into your project (or global) `CLAUDE.md`. Done.

> Windows has the same features — single-window multi-session (named pipe), biometric auth (Windows Hello), resident daemon (registry Run key). Taskbar notification instead of a Dock badge.

---

### 🔧 Build from source (developers · all platforms)

```bash
cd src-tauri && cargo build --release   # or: bun run tauri build --no-bundle
cp target/release/knock ~/.local/bin/knock
```

> ⚠️ knock is a **CLI tool**. Don't double-click a `.app` — run it with arguments like `knock annotate <md>` / `knock ask <json>`. (Running with no arguments exits immediately.)

## Claude Code plugin (skills)

After installing the CLI, add the skill plugin in Claude Code so the agent calls `knock-annotate` / `knock-ask` directly:

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
```

| Skill | Use |
|-------|-----|
| `knock-annotate` | approval / annotation gate |
| `knock-ask` | multiple-choice question (AskUserQuestion replacement) |

### Automatic plan approval (hook)

The plugin includes `PermissionRequest` + `ExitPlanMode` hooks, so **a knock window opens automatically when you exit plan mode** to review and approve the plan — **works without any CLAUDE.md guidance**. (approve → plan proceeds / request-changes·close → plan rejected + feedback)

### Agent automation (CLAUDE.md)

To have the agent raise knock for approvals / questions / web actions (`--action-url`) / Touch ID **beyond plan approval**, add the [Quick Start step 4](#quick-start-macos--apple-silicon) `CLAUDE.md` snippet to your project (or global) `CLAUDE.md`.

### (Optional) critical-bash auto-gate hook

A PreToolUse hook example that automatically raises a knock approval window right before **hard-to-undo commands** — `terraform apply/destroy`, `gh pr merge`, secret/KMS/S3 deletions, force push: [`hooks/examples/knock-critical-gate.sh`](hooks/examples/knock-critical-gate.sh). The approval window also shows a **Korean summary of what the command does** (which repo's PR is being merged, which secret-id is being deleted, etc.). Put it in `~/.claude/hooks/` and wire it into the `PreToolUse` section of `~/.claude/settings.json` (`jq` required).

> The plugin provides **skills + hooks**. Install the `knock` CLI separately via the **Install** steps above (brew / install.sh).

## Modes

### 1. annotate — approval / annotation gate

```bash
knock annotate plan.md --gate --json
```

| Option | Meaning |
|--------|---------|
| `--gate` | show an explicit `Approve` button |
| `--json` | output the result as JSON (plain text otherwise) |
| `--title T` | header title (default: filename) |
| `--touch-id` | approve via macOS Touch ID / Windows Hello (falls back to system password / button if no biometrics) |
| `--action-url <URL>` | on approval, open that URL in the browser (Scalr Apply / PR / dashboard — **action inbox**). For local files use an absolute path `file:///abs/path/mockup.html` (HTML mockups·PDF·images — documents only, executables refused). Markdown links in the body also open in the external browser on click |

**stdout contract**:

| User action | Plain | `--json` |
|-------------|-------|----------|
| Approve | `The user approved.` | `{"decision":"approved"}` |
| Close/Esc | (empty output) | `{"decision":"dismissed"}` |
| Request changes | annotation text | `{"decision":"annotated","feedback":"..."}` |

### 2. ask — multiple-choice question (AskUserQuestion replacement)

```bash
knock ask questions.json
```

The input JSON mirrors Claude Code's **AskUserQuestion schema** (+ optional `context`):

```json
{
  "context": "## Background\n\nDecision rationale (background, comparison tables, conclusion) in markdown. Rendered above the options.",
  "questions": [
    {
      "header": "Approach",
      "question": "Which way should we go?",
      "options": [
        { "label": "Option A", "description": "..." },
        { "label": "Option B", "description": "..." }
      ]
    }
  ]
}
```

- **`context` (optional)** — if a decision needs background, put markdown in the top-level `context`. It renders above the questions so the rationale is visible in the window.
- **Always checkboxes (multi-select)** — pick one or several + an "Other" free-text option. The `multiSelect` field is ignored.

Shows one question at a time (wizard), then a selection summary → submit. Always JSON output:

| Result | Output |
|--------|--------|
| Answered | `{"answers":{"Approach":["Option A","Option B"]}}` — **always string arrays** (selected labels + any Other text) |
| Closed | `{"decision":"dismissed"}` |

## Keyboard (ask questions)

| Key | Action |
|-----|--------|
| `↑` `↓` | move option focus |
| `1`~`9` | toggle that option |
| `Space` | toggle option (select/deselect) |
| `Enter` | next question |
| `→` `←` | next / previous question |
| `Cmd+Enter` | submit |
| `Esc` | close |

annotate mode: `Cmd+Enter`=approve (gate), `Esc`=close.

## Settings (knock settings)

```bash
knock settings
```

Toggle in the settings window:
- **🔒 Require Touch ID for critical gates** → saved to `~/.config/knock/config.json` as `{"touch_id": true}`. The agent reads this to apply Touch ID to important approvals like prd / IAM / destructive (no env var; set once, persists).

The settings window footer shows **Report a bug** (GitHub Issues) · **Release notes** links and the current version.

## Resident daemon (single-window multi-session)

When multiple agent sessions call knock at once, **windows don't stack — they queue into a single window**. Run the daemon at login so the menubar tray is always present and there's no first-call latency:

```bash
knock daemon install     # auto-start at login (macOS LaunchAgent / Windows registry Run key)
knock daemon status      # check whether installed
knock daemon uninstall   # remove
```

Even if not installed, the daemon spins up automatically on first call (it just won't stay resident). On a new request the Dock icon bounces and a badge (pending count) appears.

## Updates

When a new version ships, knock shows a **banner at the top of the window** (checked at startup against GitHub Releases, throttled to 24h, once per version). The banner lets you copy the `brew upgrade` command · open release notes · dismiss. Updates respect Homebrew management — **it only notifies, never auto-installs**:

```bash
brew upgrade hihenen/tap/knock
```

## Alarms

- Native OS notification (when the window opens)
- macOS Dock icon bounce (`request_user_attention`)
- Always-on-top + forced focus

## Agent workflow integration

```
# approval gate
knock annotate /tmp/approve.md --gate --json
# → proceed on {"decision":"approved"}, stop on dismissed

# question (AskUserQuestion replacement)
#  1. write the question JSON to /tmp
#  2. knock ask /tmp/q.json
#  3. parse {"answers":{...}} and branch
```

Skills: `~/.claude/skills/knock-annotate`, `~/.claude/skills/knock-ask`

## Build stack

Tauri 2 + Rust (clap, pulldown-cmark, tauri-plugin-notification) + vanilla TS.

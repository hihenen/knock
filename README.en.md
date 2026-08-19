# knock

[한국어](README.md) · **English**

### Don't miss it when your AI agent asks.

Your agent needs approval, prints a question to the terminal, and waits. But you're looking at another window. Minutes later you notice it never moved.

knock puts that question **in a window in the middle of your screen**. A notification fires, the Dock bounces, and you answer with the keyboard.

![knock approval window](docs/images/gate.png)

---

## What you can do

| | |
|---|---|
| **Get approval** | Show a plan or a diff, get approve / request-changes / cancel back |
| **Ask a question** | Show options and let them pick — number keys work |
| **Send them where it happens** | On approve, the browser jumps straight to the PR or deploy screen |
| **Lock it behind a fingerprint** | Touch ID / Windows Hello for anything hard to undo |
| **Press a real button** | Approve, cancel and scroll from a Stream Deck key |
| **Keep it in one window** | Many agents asking at once queue up instead of stacking windows |

---

## Up and running in 5 minutes

For macOS (Apple Silicon). Windows is [below](#-windows-x64).

**1. Install**

```bash
brew install hihenen/tap/knock
```

**2. Connect it to Claude Code**

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
/reload-plugins
```

**3. Keep it running**

```bash
knock daemon install
```

You get a menu bar icon, and the first call stops being slow. It works without this — it's just nicer with it.

**4. Tell your agent about it**

That's already enough for **plan approvals** to show up in a knock window. To hand over the rest, paste this into your `CLAUDE.md`.

<details>
<summary><b>Snippet for CLAUDE.md</b> (expand)</summary>

```markdown
## knock — desktop approval/question gate
- When you need **approval**, open `knock annotate <md> --gate --json` instead of asking in chat.
- For **multiple-choice questions**, use `knock ask <json>` instead of AskUserQuestion. Put
  background, comparison tables and your conclusion in the top-level `context` as markdown so
  the reasoning is visible in the window.
- When the user has to **click something on the web** (a deploy approval, a GitHub PR, a
  dashboard), pass `--action-url <URL>` so approving jumps the browser straight there.
- Use `--touch-id` for critical approvals — production, permissions, deletions.
- When the user has to **walk through several steps in a browser**, add `--checklist`. Approving
  opens the link and keeps the request in the queue as "in progress" so they can reopen the
  steps while working, then mark it done.
- To put a diagram in the body, embed `<iframe src="https://...">` (scripts run inside).
- Responses: annotate=`{"decision":"approved|annotated|dismissed"}`,
  ask=`{"answers":{"<header>":["..."]}}` (always an array).
```

</details>

**That's it.** Any session that asks now opens a window.

> New versions announce themselves with a banner in the window. `brew upgrade hihenen/tap/knock`

---

## How to use it

### Getting approval

```bash
knock annotate plan.md --gate --json
```

What the person did comes back on stdout. Branch on it.

| What they did | `--json` output |
|---|---|
| Approved | `{"decision":"approved"}` |
| Requested changes (with a note) | `{"decision":"annotated","feedback":"..."}` |
| Closed · Esc | `{"decision":"dismissed"}` |

The options you'll actually use:

| Option | When |
|---|---|
| `--gate` | Show an explicit Approve button |
| `--json` | Get JSON back (without it, a plain sentence) |
| `--title T` | Window title. Defaults to the file name |
| `--touch-id` | Approve with a fingerprint. Falls back to password or a button |
| `--action-url <URL>` | Open this URL in the browser on approve |
| `--checklist` | Keep it in the queue after approval, so a multi-step browser task can be reopened and finished |

### Asking a question

```bash
knock ask questions.json
```

Same shape as Claude Code's AskUserQuestion, so existing JSON works as-is.

```json
{
  "context": "## Background\nMarkdown shown above the options.",
  "questions": [
    {
      "header": "Direction",
      "question": "Which way should we go?",
      "options": [
        { "label": "Option A", "description": "..." },
        { "label": "Option B", "description": "..." }
      ]
    }
  ]
}
```

Answers always come back as arrays — `{"answers":{"Direction":["Option A"]}}`. Closing gives `{"decision":"dismissed"}`.

Add `multiSelect: true` to allow several picks. With a single question, it submits without a summary step.

### Putting a diagram in the gate

Embed `<iframe src="https://...">` in the body and a diagram or chart renders right in the window. JavaScript runs inside it.

```markdown
<iframe src="https://example.com/architecture.html" width="100%" height="360"></iframe>
```

Embeds render inside a box labelled **"external content"**, so they can't blend into the approval UI. Two things are enforced:

- An embed **cannot touch the approval window.** `sandbox="allow-scripts"` is forced on, and `allow-same-origin` is deliberately left off
- `<script>` and inline handlers like `onclick` in the body are **stripped**

The URL must be `http(s)`. Local files (`file:`) are blocked — use `--action-url` to open those in a browser instead.

### Pressing a real button (Stream Deck)

![knock Stream Deck keys](docs/images/streamdeck-keys.png)

Approve, cancel and scroll from physical keys. The pending count shows on the key, so you know what's waiting without looking at the screen.

```bash
cp -R com.knock.controller.sdPlugin \
  ~/Library/Application\ Support/com.elgato.StreamDeck/Plugins/
```

Copy it, then **restart the Stream Deck app** and `knock` appears in the action list.

| Action | What it does |
|---|---|
| Pending slot | Opens the Nth queued request |
| Approve · Cancel | Handles the first request |
| Scroll up · down | Moves the window body by a screenful |
| Agent session | Approves that session's pending request, or jumps to its terminal |
| Voice alerts | Toggles read-aloud |

### Keyboard

| Key | What it does |
|---|---|
| `1`–`9` | Pick an option · open that request from the queue |
| `↑` `↓` | Move between options |
| `Space` | Toggle an option |
| `Enter` | Next question (submits if there's only one) |
| `Cmd+Enter` | Submit · approve |
| `Esc` | Close |

---

## Good to know

**The window remembers its size.** It opens sized to your screen, and if you resize it by hand, it opens that way next time. Long approval bodies open nearly full height.

**Many sessions, one window.** Requests queue up, each showing its project, caller and how long it's been waiting. Number keys open them.

**Settings live in `knock settings`.**
- Always require a fingerprint to approve
- On approve, open the browser or just copy the URL (keeps tabs from piling up during a run of approvals)

**Managing the daemon:**

```bash
knock daemon install     # start at login
knock daemon status      # current state
knock daemon uninstall   # remove
```

**To gate hard-to-undo commands automatically** — there's an example hook that opens an approval window right before things like `terraform destroy`, `gh pr merge` or a secret deletion: [`hooks/examples/knock-critical-gate.sh`](hooks/examples/knock-critical-gate.sh)

---

## Install (in detail)

### 🍎 macOS (Apple Silicon)

Pick one. **Homebrew is recommended** — it runs without security warnings.

```bash
# recommended
brew install hihenen/tap/knock
```

```bash
# without Homebrew — installs to ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/hihenen/knock/master/install.sh | bash
```

```bash
# download it yourself
curl -L https://github.com/hihenen/knock/releases/latest/download/knock-macos-aarch64 -o knock
chmod +x knock
xattr -c knock          # clears the quarantine flag on downloaded files
mv knock ~/.local/bin/
```

```bash
knock --version         # check
```

### 🪟 Windows (x64)

```powershell
# recommended — installs to %LOCALAPPDATA%\knock and adds it to PATH
irm https://raw.githubusercontent.com/hihenen/knock/master/install.ps1 | iex
```

Or grab `knock-windows-x64.exe` from the [latest release](https://github.com/hihenen/knock/releases/latest) and drop it in a PATH folder.

```powershell
knock --version         # in a new PowerShell window, so PATH is picked up
```

### 🔧 Build from source

```bash
bun install
bun run build
cd src-tauri && cargo build --release
cp target/release/knock ~/.local/bin/knock
```

> knock is a **CLI tool**. You don't double-click it — you run `knock annotate <file>`.

---

## How it's built

Tauri 2 + Rust (clap, pulldown-cmark, ammonia) + vanilla TypeScript. Single binary, about 12MB.

The approval window is treated as a surface that **shows the user the truth**, so whoever writes the body is not allowed to manipulate it — hence the sanitizing and the sandbox. Full history in the [CHANGELOG](CHANGELOG.md).

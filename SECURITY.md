# Security Policy

knock acts as an **approval gate for AI coding agents** and is distributed as an
**unsigned, un-notarized** macOS binary. Understand the trust model before installing.

## Trust model

- The release binary is **ad-hoc signed but not Apple-notarized**. `install.sh` and
  the README remove the Gatekeeper quarantine bit (`xattr -c`). Only do this if you
  trust this repository and the specific release.
- **Prefer `brew install hihenen/tap/knock`** — Homebrew verifies the SHA256 pinned
  in the formula, so you are not blindly trusting a `curl | bash` download.
- **Hook mode fails safe.** If knock receives a malformed `PermissionRequest` payload,
  it does **not** auto-approve the plan — it emits nothing and lets Claude Code's
  normal permission flow handle it. A gate must never fail open.

## Verifying a manual download

```bash
shasum -a 256 knock-macos-aarch64
```

Compare the digest against the `sha256` field in
[hihenen/homebrew-tap `Formula/knock.rb`](https://github.com/hihenen/homebrew-tap/blob/master/Formula/knock.rb)
for the same version.

## Reporting a vulnerability

Please open a **private security advisory** via GitHub
(`Security` → `Report a vulnerability`) rather than a public issue.

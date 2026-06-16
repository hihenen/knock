# Changelog

All notable changes to knock are documented here. Versions follow [SemVer](https://semver.org).

## [0.1.4] - 2026-06-16

### Fixed
- **Hook output schema** now matches the official Claude Code `PermissionRequest`
  contract (`hookSpecificOutput.hookEventName` + `decision.behavior`). The previous
  shape (`decision`/`permissionDecision`) could be silently ignored.
- **Hook fails safe**: a malformed stdin payload no longer auto-approves the plan —
  it stays out of the way and lets the normal permission flow handle it. A gate must
  never fail open.
- Plan extraction now reads `tool_input.plan` (the real ExitPlanMode field) first.

### Changed
- Identity unified to `hihenen`: Cargo author, bundle id `io.github.hihenen.knock`
  (was `co.fnf.knock`).

### Added
- CI on PRs (`fmt` / `clippy` / `build` / `tsc`).
- Unit tests for the hook/annotate decision JSON and plan extraction.
- `SECURITY.md`, `CHANGELOG.md`.
- `install.sh` no longer calls the rate-limited GitHub API.

## [0.1.3] - 2026-06-16
### Added
- **Hook mode**: auto plan-approval via `PermissionRequest` + `ExitPlanMode`.
  Running `knock` with no args reads the hook payload on stdin.

## [0.1.2] - 2026-06-16
### Added
- Release automation: `git tag vX.Y.Z` → build + GitHub Release + Homebrew formula bump.

## [0.1.1] - 2026-06-16
### Added
- Menubar tray (Info / Quit), global shortcut (Cmd+Shift+K), Apple-style app icon.

## [0.1.0] - 2026-06-16
### Added
- `annotate` (approval / annotation gate) and `ask` (AskUserQuestion-style wizard) modes.
- Native window, OS notification, Dock attention, keyboard navigation.

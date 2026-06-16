# Changelog

All notable changes to knock are documented here. Versions follow [SemVer](https://semver.org).

## [0.1.6] - 2026-06-17

### Added
- **`--touch-id` 옵션** — macOS Touch ID / Windows Hello 로 승인 (robius-authentication).
  승인 액션이 생체인증을 거치고, 통과해야 `approved`. 생체 하드웨어가 없으면 시스템 암호,
  비-데스크톱은 일반 버튼으로 fallback. 변경요청/취소는 인증 없이 그대로.

## [0.1.5] - 2026-06-16

### Changed
- annotate UI 를 세로 옵션 카드로 (ask wizard 와 통일): `[1] 승인` / `[2] 인라인 입력창` / `[3] 취소`.
- 키보드 1/2/3 · ↑↓ · Cmd+Enter 일관.

### Added
- 변경요청 입력창에 **클립보드 이미지 붙여넣기** — 스크린샷을 붙이면 임시파일로 저장하고
  그 경로를 피드백에 첨부 (`save_pasted_image` command).

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

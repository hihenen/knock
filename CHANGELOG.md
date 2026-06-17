# Changelog

All notable changes to knock are documented here. Versions follow [SemVer](https://semver.org).

## [0.4.0] - 2026-06-17

### Added
- **Windows 지원** — 단일창 큐 / 데몬 / 상주가 Windows 에서도 동작한다.
  - IPC 를 `interprocess` 로 통일: macOS/Linux 는 Unix 도메인 소켓, Windows 는
    Named Pipe.
  - `knock daemon install` / `uninstall` / `status` 가 Windows 에서는 레지스트리
    Run 키(`HKCU\...\Run`)로 로그인 상주를 등록한다.
  - 생체 인증은 Windows Hello, 새 요청 시 작업표시줄 flash 로 주의 환기.

### Notes
- Windows 컴파일은 CI(windows-latest)로 검증. Dock 뱃지 숫자는 macOS/Linux 전용
  (Windows 는 트레이 + 창 flash 로 대체).

## [0.3.0] - 2026-06-17

### Added
- **대기 건수 뱃지** — 데몬 큐의 대기 요청 수를 macOS Dock 아이콘 빨간 뱃지와
  menubar 트레이 아이콘 옆 숫자로 표시한다. 새 요청이 오면 Dock 아이콘이
  통통 튀어(bounce) 주의를 환기한다.
- **LaunchAgent 상주** — `knock daemon install` / `uninstall` / `status` 로
  데몬을 로그인 시 자동 실행되게 등록한다. 등록하면 menubar 트레이가 항상
  떠 있고 첫 호출의 spawn 지연이 사라진다. (macOS 전용)

### Fixed
- LaunchAgent 로 미리 떠 있던 데몬(대기 0건 상태)에 요청이 들어올 때 창이
  빈 화면으로 남던 문제. `location.reload()` 의존을 제거하고 in-place 재렌더 +
  `queue-changed` 이벤트 + 폴링 백업으로 견고하게 바꿨다. 상세를 보는 중에는
  재렌더를 건너뛰어 입력이 날아가지 않는다.

## [0.2.0] - 2026-06-17

### Added
- **단일창 큐 (멀티 세션)** — 여러 에이전트 세션이 knock 을 동시에 호출해도
  창이 여러 개 겹치지 않고 하나의 창에 대기 목록으로 모인다. 첫 호출이 백그라운드
  데몬을 띄우고, 이후 모든 `annotate`/`ask`/hook 호출은 Unix 소켓으로 요청을
  위임한 뒤 결정을 기다린다. 데몬은 대기 요청 리스트를 보여주고, 처리하면 해당
  호출자에게만 결정을 회신한다. 데몬이 없으면 기존 단일창으로 안전하게 fallback.
- **승인 창 헤더 Touch ID 토글** — 승인 창 헤더 우측의 🔒 토글로 그 자리에서
  생체인증을 켜고 끈다. 변경은 `config.json` 에 저장되어 다음 critical 게이트와
  tray/settings 토글에도 동일하게 반영된다.

### Notes
- 큐/데몬은 Unix(macOS) 전용. Windows 는 기존 단일창 동작.

## [0.1.8] - 2026-06-17

### Added
- **메뉴바 트레이 토글** — knock 창이 떠 있을 때 menubar 트레이 아이콘 메뉴에
  `Touch ID for critical gates` 체크 항목 추가. `knock settings` 를 따로 실행하지
  않아도 클릭 한 번으로 켜고 끌 수 있으며, `~/.config/knock/config.json` 에 즉시 저장.

## [0.1.7] - 2026-06-17

### Added
- **`knock settings`** — GUI 설정 창. "critical 게이트에 Touch ID 요구" 토글을
  `~/.config/knock/config.json` (`{"touch_id": true}`) 에 저장. 환경변수 없이
  토글 한 번으로 영구 설정. 에이전트가 이 config 를 읽어 critical 게이트에 적용.

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

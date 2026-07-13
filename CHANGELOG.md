# Changelog

All notable changes to knock are documented here. Versions follow [SemVer](https://semver.org).

## [Unreleased]

## [0.5.0] - 2026-07-13

### Added
- **음성 알림 (opt-in TTS)** — 새 승인/질문 게이트가 뜰 때 소리로 읽어준다.
  자리를 비운 사이 알림을 소리로 알아채는 AFK 시나리오를 위한 기능. 기본은
  무음이라 기존 동작에 영향 없음.
  - **엔진 2종**: OS-native(macOS `say`·Windows SAPI·Linux `spd-say`/`espeak`,
    의존성 0) / 온디바이스 **Supertonic**(native Rust + ONNX 사이드카). 코어
    바이너리엔 ONNX 런타임을 링크하지 않아 오프라인·경량 유지. Supertonic 은
    `knock tts install` 로 opt-in 설치(모델 ~398MB 다운로드).
  - **낭독 범위**(`tts_scope`): 제목만 / 본문 내용까지 브리핑. 게이트 헤더의
    📄 내용 토글 또는 설정 창에서 전환.
  - **스타일**(`tts_style`): 일반 / 배달 알림(밝은 여성 보이스로 N회 반복).
    문구는 사용자 편집(`tts_phrase`, `{n}`=대기 건수, 기본 "노크 주문!"),
    반복 횟수 설정(`tts_repeat`, 1–10).
  - **보이스**(`tts_voice`): Supertonic F1–F5 / M1–M5, OS-native 는 Yuna/Samantha.
  - **토글 3층**: 게이트 헤더 🔊 소리 토글 · menubar 트레이 · 설정 창. 모두
    config `tts` 로 수렴.
  - `knock tts status | install | uninstall` 서브커맨드.

## [0.4.7] - 2026-07-07

### Added
- **ask 확인 → 실행 승인 함께 전송 (owner pre-authorization)** — `knock ask` 요약
  화면에 "이 선택을 실행 승인으로 함께 전송" 체크박스 추가(default OFF). 켜면
  답변과 함께 데몬에 **단회용·5분 TTL grant** 를 기록하고, 바로 다음에 뜨는 knock
  PermissionRequest 게이트가 창을 띄우지 않고 자동 승인(consume)함. auto mode 에서
  owner 승인 직후 실행이 게이트에 다시 막히던 흐름을 해소. grant 는 단 1회만
  소비되고 만료되면 폐기되며, TTL 정책은 webview 가 아닌 Rust(신뢰 경계)가 소유.
  데몬 미가동 시엔 grant 가 없으므로 정상 게이트로 fail-closed.
  grant 는 knock 의 **모든 게이트**가 소비한다 — `ExitPlanMode` PermissionRequest
  훅(무인자 hook 모드)과 `knock annotate --gate`(critical-gate.sh 가 호출하는 위험
  작업 승인)를 모두 창 없이 자동 통과. 단, knock 이 게이트하지 않는 작업(예: knock
  필터에 안 걸리는 일반 Bash, 또는 Claude Code 자체 auto-mode classifier 가 막는
  건)은 이 grant 로 통과되지 않는다 — 그 경로는 knock 밖이기 때문.
- **`open_url` 토글** — menubar 트레이에 "Open action URL on approve" 체크박스 추가
  (default ON, `~/.config/knock/config.json` 의 `{"open_url": false}` 로 OFF 영구
  저장). OFF 면 승인 시 `--action-url` 자동 점프 안 함 + URL 을 클립보드에 복사 →
  다수 게이트 연속 승인 시 브라우저 탭 폭주 회피. 본문 markdown 링크 클릭은
  토글과 무관. `touch_id` 토글과 동일 패턴.

## [0.4.6] - 2026-06-24

### Fixed
- **승인 시 브라우저 탭이 여러 개 열리고 이전 항목의 링크가 열리던 버그** — 데몬
  단일창에서 큐로 여러 건을 처리할 때, 승인/질문 화면의 정적 버튼에 리스너가
  누적되어 N번째 승인 시 이전 항목들의 `approve`(각자의 stale `--action-url`)가
  함께 발화하던 문제. 재렌더 전에 리스너를 가진 요소를 clone-replace 해 누적
  리스너를 제거. approve 에도 1회 가드 추가(키보드+클릭 중복 방지).

### Added
- **critical 게이트 승인 창에 한글 요약** — `gh pr merge`/시크릿 삭제/terraform
  apply 등 위험 명령이 무엇을 하는지(어느 repo 의 어떤 PR 머지, 어떤 secret-id
  삭제, 어떤 S3 버킷 등) 대상까지 한글로 요약해 한눈에 승인 판단 가능
  (`hooks/examples/knock-critical-gate.sh`).
- README 설치 가이드를 macOS / Windows 자기완결 복붙 흐름으로 완전 분리.

## [0.4.5] - 2026-06-19

### Added
- **로컬 문서 열기 (`file://`)** — annotate 본문의 markdown 링크나 `--action-url`
  에 `file:///abs/path/mockup.html` 같은 로컬 경로를 주면 외부 브라우저로 엽니다
  (HTML 목업·PDF·이미지 등 검토용). 보안상 **문서/이미지 확장자만 허용**하고
  실행 파일(.app/.sh 등)은 거부합니다. http(s) 는 그대로 동작.

### Notes
- ask 의 "기타(직접 입력)" 옵션은 모든 질문에 자동 포함되어 있습니다 — 선택하면
  자유 텍스트 입력칸이 열리고 답변 배열에 포함됩니다.

## [0.4.4] - 2026-06-18

### Added
- **업데이트 알림** — 시작 시 GitHub Releases `latest` 를 확인(24h throttle, 실패
  시 silent)하고, 새 버전이 있으면 상단에 비침습 배너를 띄운다. `brew upgrade`
  명령 복사 + 릴리스 노트 링크 + 버전별 닫기(다시 안 뜸). Tauri 자동 설치는
  Homebrew 와 충돌하므로 *알림만, 설치는 brew*.
- **릴리스 노트 링크** — settings 창에 "📋 릴리스 노트" + 현재 버전 표시.
- **GitHub Release 노트 자동 첨부** — release 워크플로가 CHANGELOG 의 해당 버전
  섹션을 release body 로 넣는다 (커밋 자동 노트와 함께).

## [0.4.3] - 2026-06-18

### Added
- **`--action-url` (action inbox)** — annotate 승인 시 지정한 URL 을 브라우저로
  자동 오픈. knock 을 "행동 inbox" 로: 승인 한 번에 Scalr Apply / GitHub PR /
  ArgoCD 등 다음 행동 지점으로 점프한다. 승인 버튼 라벨에 `→ 링크 열기` 표시.
- **본문 markdown 링크 clickable** — annotate/ask context 안의 http(s) 링크를
  클릭하면 webview 가 아니라 실제 브라우저로 열린다 (`open_url`).
- **버그 신고** — settings 창에 "🐞 버그 신고" 링크 + 현재 버전 표시. GitHub
  Issues 로 연결. `.github/ISSUE_TEMPLATE` 에 bug / feature 양식 추가.

## [0.4.2] - 2026-06-18

### Fixed
- **데몬 창이 안 뜨던 버그** — 데몬이 이미 떠 있는데 새 요청이 올 때, 소켓
  listener 스레드에서 `window.show()` 를 호출해 macOS 가 (UI 는 메인 스레드만)
  silently no-op 하던 문제. show + 뱃지 갱신을 `run_on_main_thread` 로 메인
  스레드에 dispatch 하도록 수정. 멀티세션에서 창이 안정적으로 뜬다.

### Changed
- **ask 모드를 항상 multi-select(체크박스)로 통일** — 실사용은 "1~2개 선택 +
  기타에 메모" 가 더 흔해, 단일 라디오로는 표현이 안 됐다. single radio 폐지,
  옵션은 모두 체크박스, 출력은 항상 label 배열(`{"answers":{"<header>":["..."]}}`).

## [0.4.1] - 2026-06-18

### Added
- **ask 모드 맥락(context) 본문** — ask JSON 에 `context` (markdown) 필드를 넣으면
  질문 위에 배경/비교표/결론이 렌더된다. 결정 근거를 창 안에서 바로 볼 수 있다.
- **ask 모드 Touch ID 토글** — 승인 창과 동일하게 ask 헤더에도 🔒 토글. 켜면
  제출에 생체 인증을 건다.
- 창 크기 확대(1120×980) + 본문 스크롤바 상시 표시(긴 markdown 대비). context 와
  질문이 하나의 스크롤 영역에서 함께 스크롤된다.

### Fixed
- **데몬 중복 버그** — `interprocess` 의 `try_overwrite(true)` 가 *살아있는*
  데몬의 소켓까지 덮어써 여러 세션에서 데몬이 중복으로 뜨던 문제. 살아있는
  데몬이 있으면 새 데몬이 양보하도록 단일 데몬을 보장한다.

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

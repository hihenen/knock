# Changelog

All notable changes to knock are documented here. Versions follow [SemVer](https://semver.org).

## [Unreleased]

### Added
- **Stream Deck 스크롤 키 2종** (`위로 스크롤` / `아래로 스크롤`) — 내용이 긴 게이트는 한
  화면에 안 들어오는데, 읽으려고 트랙패드로 손이 가면 물리 키로 결정한다는 전제가 깨진다.
  IPC 에 `scroll` 종류를 더해 창 본문을 한 화면(85%)씩 굴린다. 열려 있는 뷰 중 **실제로
  넘치는** 것만 굴리므로 짧은 본문에서는 아무 일도 일어나지 않는다.

### Changed
- **창 크기를 화면 기준으로 잡고, 손으로 조절한 크기를 기억한다** — 고정 픽셀(`large`
  1120x900)이라 큰 화면에서는 내용이 풍성한 게이트가 레터박스로 열렸다. 이제 사용 가능한
  화면 영역(메뉴 막대·독 제외) 기준으로 열고 — 긴 본문은 세로 94% — 창을 직접 조절하면
  그 크기를 레이아웃별로 기억해 다음에 그대로 연다. 결정 대상이 화면 밖에 있으면 게이트가
  제 일을 못 한다.

## [0.6.3] - 2026-08-19

### Added
- **게이트 본문에 HTML 임베드(`<iframe>`) 정식 지원** — 생성된 다이어그램·차트를 승인 창
  안에서 바로 볼 수 있다. 스크립트가 도는 임베드(mermaid, 차트 라이브러리)를 의도적으로
  허용하되 `sandbox="allow-scripts"` 를 **강제**하고 `allow-same-origin` 은 주지 않는다 —
  둘을 함께 주면 프레임이 자기 샌드박스를 풀고 부모 문서, 즉 승인 UI 자체에 닿는다.
  작성자가 직접 넣은 `sandbox` 는 권한을 넓힐 수 없다(우리 속성을 앞에 주입해 먼저 이긴다).
- **임베드에 테두리 + "외부 콘텐츠" 라벨 + 높이 상한** — 임베드는 자기 박스 안에 무엇이든
  그릴 수 있고 여기엔 가짜 승인 헤더도 포함된다. 경계를 눈에 보이게 만들고, 액션 바를
  화면 밖으로 밀어내지 못하게 한다.

### Fixed
- **ASCII 다이어그램이 접혀서 깨지던 문제** — `pre` 는 가로 스크롤인데 `pre code` 가
  `pre-wrap` + `break-word` 로 강제 줄바꿈해 박스 드로잉이 행 중간에서 접혔다. 창이 좁은
  게이트에서는 거의 항상 발동했다. 줄을 보존하고 부모가 스크롤한다.
- **넓은 표가 셀을 짓눌러 읽을 수 없던 문제** — 표를 자체 스크롤 박스에 넣고, 셀 폭 상한을
  둬 긴 산문은 적당한 길이에서 접히고 짧은 열은 좁게 유지된다.

### Security
- **렌더된 게이트 HTML 살균(ammonia)** — 이전에는 raw HTML 이 `pulldown-cmark` → `innerHTML`
  → `csp: null` 세 관문을 그대로 통과했다. 인라인 이벤트 핸들러(`onclick` 등)와 `<script>`
  가 제거된다. 게이트 창은 *승인을 받는 화면*이라, 그 본문을 만든 쪽이 창을 조작할 수
  있으면 게이트가 보증하는 것이 없어진다.

## [0.6.2] - 2026-08-18

### Added
- **세션 `stale` 상태** — `working` 인데 5분 넘게 출력이 없으면 빨강 + `⚠`. 나머지 상태가
  "할 일이 있다" 라면 이건 "고장났다" 라서 우선순위가 제일 위다. 돈다고 표시된 세션이
  실제로는 멈춰 있으면 화면상 조용한 세션과 구별되지 않아 영영 안 보인다.

  `active` 는 stale 로 보지 않는다 — 터미널이 붙어 있다는 뜻이지 작업 중이라는 뜻이 아니라,
  그냥 열어둔 터미널까지 빨갛게 만들면 소음이 된다. 대신 경과 시간은 그대로 표시된다.

### Changed
- **승인 / 취소 / 음성 키가 대기 0건일 때도 정체를 유지** — 기존에는 `key-empty`(글자 없는
  사각형)로 떨어져 무슨 키인지 알 수 없었다. 색만 죽이고 글자는 남긴다.


### Added
- **세션 키에 승인 대기 건수** — 그 세션이 올린 요청이 있으면 `live N` 대신 `승인 N` 을
  띄우고 색으로 강조한다. 눌러야 할 이유가 그쪽이다. 상태 우선순위는
  승인 대기 > 안 본 출력 > `working` > 그 외.

  렌더와 클릭이 같은 규칙(`pendingFor`)을 쓴다. 키에 "승인 2" 라고 떠 있는데 눌렀을 때
  다른 걸 집으면 안 되기 때문이다 — 물리 버튼은 확인 없이 누르는 물건이다.


## [0.6.1] - 2026-08-18

### Added
- **Stream Deck `에이전트 세션` 액션** — orca 워크트리 기준으로 도는 세션을 키에 띄운다.
  이름 · 마지막 활동 이후 경과 · live 터미널 수. 상태는 색으로 구분한다(안 본 출력 주황 /
  `working` 초록 / 대기 남색). 누르면 **그 세션이 올린 승인 대기가 있으면 승인**하고, 없으면
  그 터미널 탭으로 전환한다.

  "얼마나 돌고 있는지" 는 워크트리 생성 시각이 아니라 `lastOutputAt` 기준이다. 전자는 작업이
  오래됐다는 뜻일 뿐이고(실측: 39일), **`working` 인데 마지막 출력이 30분 전** 이라는 신호가
  실제로 봐야 할 것이다.

  orca 가 없으면 이 액션만 비어 보이고 나머지는 그대로 동작한다.


## [0.6.0] - 2026-08-18

### Added
- **`knock ctl`** — 외부 컨트롤러용 서브커맨드. `list` / `focus [id]` / `approve [id]` /
  `dismiss [id]` / `tts`. id 자리에 `@2` 를 쓰면 큐의 N번째를 가리킨다(물리 키는 id 를
  모르므로 자리로 지정). 데몬이 없으면 창을 띄우지 않고 종료한다.
- **Stream Deck 플러그인** (`com.knock.controller.sdPlugin`) — 대기 슬롯 / 승인 / 취소 /
  음성 알림 4종 액션. 슬롯 키에는 제목이 아니라 **요청 출처**(프로젝트 · 호출자 · 대기시간)를
  띄운다. 여러 Claude Code / Codex 세션이 동시에 대기할 때 구분해야 할 것이 그쪽이기
  때문이다. 슬롯 번호는 슬롯 키들끼리의 순서로 자동 부여되어, 어디에 놓든 왼쪽부터 @1 이다.
- **`external_skip_touch_id`** (config, 기본 false) — 물리 키 승인만 Touch ID 를 면제한다.
  `touch_id` 를 통째로 끄면 화면 승인까지 무방비가 되므로 두 경로를 나눌 수 있게 했다.
- **`annotate --checklist`** — 승인 후에도 요청을 큐에 "진행 중"으로 남긴다. 승인하면
  `--action-url` 이 열리고 창은 닫혀 브라우저를 가리지 않으며, 트레이에서 다시 열어 절차를
  확인하고 끝나면 "완료"를 누른다. 그때 `{"decision":"approved","completed":true}` 로
  resolve 된다. 기존에는 승인 즉시 창이 닫혀 절차를 다시 볼 수 없었고, 작업을 끝냈다는
  신호를 호출자가 받을 방법도 없었다. 큐가 없는 단일창 모드에서는 기존대로 즉시 resolve.

### Changed
- **승인 흐름 UX 정리** — 대기 목록에 번호·프로젝트·호출자·대기 시간을 표시하고
  `1`~`9` 숫자키로 바로 연다. 단일 질문은 요약 단계를 건너뛰며, 헤더는 음성
  on/off와 Touch ID만 남겨 단순화했다. 화면 내용에 맞춰 창 크기도 더 작게 조절한다.
- **Touch ID 동작 일관화** — 전역 설정은 승인과 질문 모두에 적용하고, 명시적인
  `--touch-id` 요청은 전역 설정이 꺼져 있어도 인증을 요구한다.

### Fixed
- **데몬 상세창 닫기 교착** — macOS 빨간 닫기 버튼으로 상세 요청을 닫으면 단순히
  창만 숨기지 않고 해당 요청을 `dismissed`로 해제한다. 대기 목록 닫기는 기존처럼
  상주 데몬 창만 숨긴다.

## [0.5.2] - 2026-07-13

### Fixed
- **여러 노크 동시 도착 시 TTS 겹침/꼬임** — 각 알림이 개별 스레드로 동시에
  재생돼 소리가 뒤엉키던 문제. 이제 전용 워커 스레드가 **한 번에 하나씩만**
  재생하고, 짧은 시간에 몰린 알림은 **최신 것만**(대기 건수는 누적이므로) 재생해
  파일업/겹침을 없앤다.

## [0.5.1] - 2026-07-13

### Added
- **게이트 헤더 음성/반복 빠른 컨트롤** — 게이트 창 헤더에 음성 드롭다운
  (기본·F1~F5·M1~M5)과 반복 횟수 입력칸을 추가. 설정 창을 열지 않아도
  낭독 음성과 배달 알림 반복 횟수를 그 자리에서 바꿀 수 있고, 변경 즉시
  config(`tts_voice`·`tts_repeat`)에 저장되어 다음 게이트부터 반영된다.

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

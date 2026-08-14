# knock `open_url` 토글 제안 (read-only 검토)

승인 시 `--action-url` 자동 점프를 사용자 설정으로 켜고 끄자는 제안. `touch_id` 토글이 이미 같은 패턴으로 들어가 있어 그대로 가져다 쓰면 됨.

## 현황 — action-url 동작

knock v0.4.6 기준, 승인 시 무조건 브라우저 점프.

- CLI 인자: `src-tauri/src/lib.rs:60,111` — `Mode::Annotate { action_url: Option<String> }` 로 파싱, 프론트에 `"actionUrl"` 로 전달 (lib.rs:276).
- 프론트 처리: `src/main.ts:275-277` — 승인 closure 안에서 `if (p.actionUrl) await invoke("open_url", { url: p.actionUrl })`. 무조건 호출, 토글 없음.
- 실제 열기: `src-tauri/src/lib.rs:356-389` — `open_url` Tauri command 가 macOS `open`, Linux `xdg-open`, Windows `start` 호출.

이미 `src/main.ts:84` 의 본문 링크 클릭(`href`)도 같은 `open_url` 명령을 쓰므로, 토글이 *승인 시 자동 점프*만 막아야 함 (본문 링크 클릭은 항상 살아 있어야 함).

## 설계 — `touch_id` 패턴 그대로

config 파일·GUI·CLI flag 셋 다 기존 `touch_id` 구조를 1:1 미러링.

| 측면 | touch_id (기존) | open_url (제안) |
|---|---|---|
| config key | `~/.config/knock/config.json` `"touch_id": bool` (default false) | `"open_url": bool` **default true** (기존 동작 보존) |
| reader fn | `config_touch_id()` (lib.rs:99) | `config_open_url()` (신규, 기본값만 `true`) |
| setter fn + Tauri cmd | `set_config_touch_id` / `save_touch_id` | `set_config_open_url` / `save_open_url` |
| tray 메뉴 | `CheckMenuItemBuilder::with_id("touch_id", "Touch ID for critical gates")` (lib.rs:632, 730) | `"open_url"`, "Open action URL on approve" 한 줄 추가 |
| 프론트 state 전달 | `"configTouchId": config_touch_id()` (lib.rs:275,283,287) | `"configOpenUrl": config_open_url()` 추가 |
| 프론트 사용처 | `tdToggle.checked` (main.ts:268) | `main.ts:275` 의 `if (p.actionUrl)` 를 `if (p.actionUrl && p.configOpenUrl)` 로 |
| CLI flag (선택) | `--touch-id` 플래그 | 별도 flag 불필요 — config 만으로 충분. 굳이 추가하면 `--open-url` / `--no-open-url` |

### 승인 후 동작

- `open_url: true` (default) → 현재와 동일. 자동 점프.
- `open_url: false` → 승인은 정상 처리. URL 은 **자동으로 열리지 않음**. 다음 둘 중 하나로 사용자 인지:
  - (a) 승인 직전/직후 본문에 URL 표시 + 한 줄 안내 ("승인 후 자동 열기 OFF — 직접 열어주세요").
  - (b) 추가로 클립보드 복사 (Tauri clipboard plugin). 옵션, 1-스텝 추가.
- **본문 markdown 안 링크 클릭은 그대로 동작** — `main.ts:84` 의 href 핸들러는 토글과 무관.

### 에이전트 사용

기존 호출 `knock annotate <md> --gate --action-url <URL>` 변경 0. config 값이 결정. 에이전트는 owner 토글 상태를 알 필요 없음.

## 구현 옵션

| 옵션 | 변경 위치 | 장점 | 단점 |
|---|---|---|---|
| **A. knock 본체에 토글** | `src-tauri/src/lib.rs` (config getter·setter·tray 메뉴) + `src/main.ts` (승인 closure 조건) | touch_id 와 일관, GUI 토글 + 영구 저장, 모든 호출에 자동 적용 | knock 빌드·재배포 필요 |
| **B. 환경변수 우회** | `main.ts` 승인 closure 에서 `import.meta.env.VITE_KNOCK_NO_OPEN_URL` 또는 Rust 측 `env::var("KNOCK_NO_OPEN_URL")` 체크 | 빠름, 빌드 한 번 | GUI 토글 없음, 호출 시마다 env 세팅 필요. owner 가 영구 OFF 의도면 불편 |
| **C. 에이전트 측 우회** | knock 변경 0. 에이전트가 `--action-url` 빼고 markdown 본문에 URL 만 표시 | knock 빌드 불필요 | 자동 점프 가치 통째 상실 — 항상 수동. 토글 의미 없음 (껐다 켤 수 없음) |

**권장**: **A**. 변경 라인 ~20줄, touch_id 패턴 카피. tray UI 일관성·영구 저장·on/off 자유 모두 만족.

## A안 구현 위치 (상세)

1. `src-tauri/src/lib.rs`
   - `config_open_url()` 신규 (default `true`).
   - `set_config_open_url(enabled)` + Tauri command `save_open_url`.
   - `state_for_frontend` (lib.rs:268~) 의 JSON 에 `"configOpenUrl": config_open_url()` 추가.
   - `tauri::generate_handler![...]` (lib.rs:533, 683) 에 `save_open_url` 추가.
   - tray 메뉴 builder (lib.rs:632, 730) 에 `CheckMenuItem` 한 줄 + 토글 핸들러 한 케이스 추가.
2. `src/main.ts`
   - `Payload` 인터페이스에 `configOpenUrl?: boolean` 추가.
   - 승인 closure (`main.ts:275`): `if (p.actionUrl && (p.configOpenUrl ?? true))` 로 가드.
   - (선택) 토글 OFF 일 때 승인 버튼 라벨에 "(URL 자동 열기 OFF)" 힌트.
3. `README.md` / `CHANGELOG.md`: 토글 설명 한 단락. PRCS rules `agent-workflow.md` 의 action-url 섹션도 "토글 OFF 시 자동 점프 안 됨" 1줄 추가.

## UX flow

- **자동 (default)**: 게이트 등장 → 승인 → 브라우저 새 탭으로 URL 점프. 현재와 동일.
- **수동 (토글 OFF)**: 게이트 등장 → owner 가 본문에서 URL 확인 (markdown 링크 클릭으로 미리 열어볼 수도 있음) → 승인 → 창 닫힘, 자동 점프 없음. owner 가 직접 다시 가서 클릭 (또는 본문 링크 클릭으로 이미 열어둠).

수동 모드의 가치: owner 가 게이트 처리 중인 화면 컨텍스트(다른 작업·집중 중)에서 브라우저로 강제 전환되는 흐름 깨짐을 방지. 특히 다수 게이트 연속 승인 시 탭 폭주 회피.

## 주의

- 위 lib.rs 라인 번호는 v0.4.6 (`/Users/yun/work/ai/knock`) 기준. 다음 릴리스에서 shift 가능.
- 본문 markdown 안 링크 클릭(`main.ts:84`, `:146`, `:709`, `:715`)은 토글과 분리 — 사용자가 명시적으로 클릭하는 행위라 막을 이유 없음.
- knock 코드는 본 검토에서 수정하지 않음. owner 결정 후 별도 PR.

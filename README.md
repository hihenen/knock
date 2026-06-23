# knock

**한국어** · [English](README.en.md)

AI 코딩 에이전트를 위한 데스크톱 승인 / 주석 / 질문 게이트.
브라우저 탭이 아닌 **Tauri 네이티브 창** + OS 알림 + Dock 두드림 + 키보드 네비.

- 단일 바이너리 (~9.5MB), Bun/Tauri 빌드
- 단순한 `stdout` 계약 → 기존 스킬·훅·CLI 에 그대로 연결
- 채팅 blocking 질문(AskUserQuestion) 을 데스크톱 창으로 대체

## Quick Start (macOS · Apple Silicon)

**1. CLI 설치** — 둘 중 하나

```bash
brew install hihenen/tap/knock                                                       # 권장 (경고 없음)
curl -fsSL https://raw.githubusercontent.com/hihenen/knock/master/install.sh | bash  # 또는 (Homebrew 없이)
```

**2. Claude Code 플러그인** (스킬 + hook)

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
/reload-plugins
```

**3. 데몬 상주** (필수)

```bash
knock daemon install   # 로그인 시 데몬 상주 → menubar 트레이 항상 표시 + 첫 호출 지연 0
```

여러 세션이 동시에 호출해도 한 창에 큐로 모이고, menubar 트레이가 항상 떠 있어 대기 건수(뱃지)를 바로 봅니다. Touch ID 는 승인 창 헤더의 🔒 토글로 그 자리에서 켜면 됩니다.

**4. 에이전트 자동화** — plan 승인은 hook 으로 자동이지만, **그 외 승인·질문·웹 행동까지** knock 으로 띄우게 하려면 프로젝트(또는 글로벌) `CLAUDE.md` 에 아래를 추가하세요:

```markdown
## knock — 데스크톱 승인/질문 게이트
- 사용자에게 **승인**이 필요하면 채팅 대신 `knock annotate <md> --gate --json` 게이트를 띄운다.
- **객관식 질문**은 AskUserQuestion 대신 `knock ask <json>`. JSON 최상위 `context` 에
  배경·비교표·결론을 markdown 으로 담아 결정 근거를 창에서 바로 보게 한다.
- 사용자가 **웹에서 클릭·승인**해야 하면(Scalr Apply / GitHub PR / ArgoCD / 대시보드 등)
  `--action-url <URL>` 을 넣어 승인 한 번에 브라우저로 그 행동 지점으로 점프하게 한다.
- prd·IAM·destructive 같은 critical 승인엔 `--touch-id` (knock settings 로 켠 경우).
- knock 응답: annotate=`{"decision":"approved|annotated|dismissed"}`, ask=`{"answers":{"<h>":["..."]}}`(항상 배열).
```

**여기까지면 끝** — 설치 → 플러그인 → 데몬 상주 → 에이전트 지침이 한 흐름입니다. 이제 어느 세션이든 knock 으로 승인·질문하고, 여러 세션이 한 창에 큐로 모입니다.

> **업데이트**: 새 버전이 나오면 knock 창 상단 배너로 알립니다. `brew upgrade hihenen/tap/knock` (+ `/plugin marketplace update knock` → `/reload-plugins`)

---

## 설치 (자세히)

> 두 갈래입니다. **본인 OS 섹션만** 위에서 아래로 복붙하면 끝납니다. (macOS / Windows 가 안 섞이게 완전히 분리)

---

### 🍎 macOS (Apple Silicon) — 처음부터 끝까지

**1단계 · CLI 설치** (셋 중 하나 — Homebrew 권장)

```bash
# (권장) Homebrew — Gatekeeper quarantine 자동 처리, 경고 없음
brew install hihenen/tap/knock
```

```bash
# (대안) Homebrew 없이 — ~/.local/bin/knock 에 설치 + quarantine 자동 제거
curl -fsSL https://raw.githubusercontent.com/hihenen/knock/master/install.sh | bash
```

```bash
# (대안) 바이너리 직접 다운로드
curl -L https://github.com/hihenen/knock/releases/latest/download/knock-macos-aarch64 -o knock
chmod +x knock
xattr -c knock                 # Gatekeeper quarantine 제거 (다운로드 빌드라 필요)
mv knock ~/.local/bin/
```

```bash
# 설치 확인
knock --version
```

**2단계 · Claude Code 플러그인** (스킬 + 자동 plan 승인 hook) — Claude Code 안에서:

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
/reload-plugins
```

**3단계 · 데몬 상주** (필수 — 로그인 시 자동 실행 / menubar 트레이 / 첫 호출 지연 0)

```bash
knock daemon install
knock daemon status            # 설치 확인
```

**4단계 · 에이전트 지침** — 프로젝트(또는 글로벌 `~/.claude/CLAUDE.md`) `CLAUDE.md` 에 [Quick Start 4단계](#quick-start-macos--apple-silicon)의 스니펫을 붙여넣으면 끝.

---

### 🪟 Windows (x64) — 처음부터 끝까지

**1단계 · CLI 설치** (PowerShell — 둘 중 하나)

```powershell
# (권장) install.ps1 — knock.exe 를 %LOCALAPPDATA%\knock 에 설치 + user PATH 추가
irm https://raw.githubusercontent.com/hihenen/knock/master/install.ps1 | iex
```

```powershell
# (대안) 바이너리 직접 — releases 에서 knock-windows-x64.exe 받아 PATH 폴더에 둡니다
#   https://github.com/hihenen/knock/releases/latest  →  knock-windows-x64.exe
```

```powershell
# 설치 확인 (새 PowerShell 창에서 — PATH 갱신 반영)
knock --version
```

**2단계 · Claude Code 플러그인** (스킬 + 자동 plan 승인 hook) — Claude Code 안에서:

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
/reload-plugins
```

**3단계 · 데몬 상주** (필수 — 로그인 시 자동 실행 / 작업표시줄 상주 / 첫 호출 지연 0)

```powershell
knock daemon install
knock daemon status            # 설치 확인 (레지스트리 Run 키)
```

**4단계 · 에이전트 지침** — 프로젝트(또는 글로벌) `CLAUDE.md` 에 [Quick Start 4단계](#quick-start-macos--apple-silicon)의 스니펫을 붙여넣으면 끝.

> Windows 도 기능 동일 — 멀티세션 단일창(named pipe), 생체 인증(Windows Hello), 데몬 상주(레지스트리 Run 키). Dock 뱃지 대신 작업표시줄 알림.

---

### 🔧 소스 빌드 (개발자용 · 모든 플랫폼)

```bash
cd src-tauri && cargo build --release   # 또는: bun run tauri build --no-bundle
cp target/release/knock ~/.local/bin/knock
```

> ⚠️ knock 은 **CLI 도구**입니다. `.app` 더블클릭이 아니라 `knock annotate <md>` / `knock ask <json>` 처럼 인자와 함께 실행합니다. (인자 없이 실행하면 즉시 종료)

## Claude Code 플러그인 (스킬)

CLI 설치 후, Claude Code 에서 스킬 플러그인을 추가하면 에이전트가 `knock-annotate` / `knock-ask` 를 직접 호출합니다:

```
/plugin marketplace add hihenen/knock
/plugin install knock@knock
```

| 스킬 | 용도 |
|------|------|
| `knock-annotate` | 승인 / 주석 게이트 |
| `knock-ask` | 객관식 질문 (AskUserQuestion 대체) |

### 자동 plan 승인 (hook)

플러그인에는 `PermissionRequest` + `ExitPlanMode` hook 이 포함되어, **plan mode 를 빠져나갈 때 자동으로 knock 창**이 떠서 plan 을 검토·승인합니다 — **CLAUDE.md 지침 없이도 동작**. (승인 → plan 진행 / 변경요청·닫기 → plan 거부 + 피드백)

### 에이전트 자동화 (CLAUDE.md)

plan 승인 외의 **승인·질문·웹 행동(`--action-url`)·Touch ID** 까지 에이전트가 knock 으로 띄우게 하려면, 위 [Quick Start 4단계](#quick-start-macos--apple-silicon)의 `CLAUDE.md` 스니펫을 프로젝트(또는 글로벌) `CLAUDE.md` 에 추가하세요.

### (선택) critical bash 자동 게이트 hook

`terraform apply/destroy`, `gh pr merge`, 시크릿·KMS·S3 삭제, 강제 push 같은 **되돌리기 어려운 명령**을 실행 직전 자동으로 knock 승인 창에 띄우는 PreToolUse hook 예제: [`hooks/examples/knock-critical-gate.sh`](hooks/examples/knock-critical-gate.sh). 승인 창에 **명령이 무엇을 하는지 한글 요약**(어느 repo 의 어떤 PR 머지, 어떤 시크릿 삭제 등)이 함께 표시됩니다. `~/.claude/hooks/` 에 두고 `~/.claude/settings.json` 의 `PreToolUse` 에 연결하세요 (`jq` 필요).

> 플러그인은 **스킬 + hook** 을 제공합니다. `knock` CLI 는 위 **설치** 단계(brew / install.sh)로 따로 설치하세요.

## 모드

### 1. annotate — 승인 / 주석 게이트

```bash
knock annotate plan.md --gate --json
```

| 옵션 | 의미 |
|------|------|
| `--gate` | 명시적 `승인` 버튼 노출 |
| `--json` | 결과를 JSON 으로 출력 (없으면 평문) |
| `--title T` | 헤더 제목 (기본: 파일명) |
| `--touch-id` | macOS Touch ID / Windows Hello 로 승인 (생체 없으면 시스템 암호 / 버튼 fallback) |
| `--action-url <URL>` | 승인 시 브라우저로 그 URL 자동 오픈 (Scalr Apply / PR / 대시보드 — **action inbox**). 로컬 문서는 절대경로 `file:///abs/path/mockup.html` (HTML 목업·PDF·이미지 — 문서만, 실행 파일 거부). 본문 markdown 링크도 클릭 시 외부 브라우저로 열림 |

**stdout 계약**:

| 사용자 행동 | 평문 | `--json` |
|------------|------|----------|
| 승인 | `The user approved.` | `{"decision":"approved"}` |
| 닫기/Esc | (빈 출력) | `{"decision":"dismissed"}` |
| 변경요청 | 주석 텍스트 | `{"decision":"annotated","feedback":"..."}` |

### 2. ask — 객관식 질문 (AskUserQuestion 대체)

```bash
knock ask questions.json
```

입력 JSON 은 Claude Code 의 **AskUserQuestion 스키마와 동형** (+ 선택적 `context`):

```json
{
  "context": "## 배경\n\n결정 근거(배경·비교표·결론)를 markdown 으로. 선택지 위에 렌더된다.",
  "questions": [
    {
      "header": "구현 방향",
      "question": "어느 방향으로 갈까?",
      "options": [
        { "label": "A안", "description": "설명..." },
        { "label": "B안", "description": "설명..." }
      ]
    }
  ]
}
```

- **`context` (선택)** — 결정에 배경이 필요하면 최상위 `context` 에 markdown 을 담는다. 질문 위에 렌더되어 근거를 창에서 바로 본다.
- **항상 체크박스(multi-select)** — 1개~여러 개 선택 + "기타" 자유입력. `multiSelect` 필드는 무시.

한 질문씩(wizard) 보여주고 마지막에 선택 요약 → 제출. 항상 JSON 출력:

| 결과 | 출력 |
|------|------|
| 답변 | `{"answers":{"구현 방향":["A안","B안"]}}` — **항상 string 배열** (선택 label + 기타 텍스트) |
| 닫기 | `{"decision":"dismissed"}` |

## 키보드 (ask 질문)

| 키 | 동작 |
|----|------|
| `↑` `↓` | 옵션 포커스 이동 |
| `1`~`9` | 해당 옵션 토글 |
| `Space` | 옵션 토글 (선택/해제) |
| `Enter` | 다음 질문 |
| `→` `←` | 다음 / 이전 질문 |
| `Cmd+Enter` | 제출 |
| `Esc` | 닫기 |

annotate 모드: `Cmd+Enter`=승인(gate), `Esc`=닫기.

## 설정 (knock settings)

```bash
knock settings
```

설정 창에서 토글:
- **🔒 critical 게이트에 Touch ID 요구** → `~/.config/knock/config.json` 의 `{"touch_id": true}` 로 저장. 에이전트가 이 값을 읽어 prd/IAM/destructive 같은 중요 승인에 Touch ID 를 적용 (환경변수 불필요, 한 번만 켜면 영구).

설정 창 하단에 **버그 신고**(GitHub Issues) · **릴리스 노트** 링크와 현재 버전이 표시됩니다.

## 데몬 상주 (멀티세션 단일창)

여러 에이전트 세션이 동시에 knock 을 호출해도 **창이 여러 개 겹치지 않고 하나의 창에 대기 목록(큐)** 으로 모입니다. 로그인 시 데몬을 상주시키면 menubar 트레이가 항상 떠 있고 첫 호출 지연이 사라집니다:

```bash
knock daemon install     # 로그인 시 자동 실행 (macOS LaunchAgent / Windows 레지스트리 Run 키)
knock daemon status      # 설치 여부 확인
knock daemon uninstall   # 해제
```

미설치 시에도 첫 호출 때 데몬이 자동으로 떠서 동작합니다(상주만 안 할 뿐). 새 요청이 오면 Dock 아이콘이 튀고(bounce) 뱃지 숫자(대기 건수)가 표시됩니다.

## 업데이트

새 버전이 나오면 knock 창 상단에 **배너로 알립니다** (시작 시 GitHub Releases 확인, 24h 간격, 버전별로 한 번만). 배너에서 `brew upgrade` 명령 복사 · 릴리스 노트 열기 · 닫기 가능. 업데이트는 Homebrew 관리를 존중해 **자동 설치하지 않고 안내만** 합니다:

```bash
brew upgrade hihenen/tap/knock
```

## 알람

- OS 네이티브 알림 (창 띄울 때)
- macOS Dock 아이콘 튕김 (`request_user_attention`)
- 항상 위(always-on-top) + 포커스 강제

## 에이전트 워크플로우 연동

```
# 승인 게이트
knock annotate /tmp/approve.md --gate --json
# → {"decision":"approved"} 받으면 진행, dismissed 면 중단

# 질문 (AskUserQuestion 대체)
#  1. 질문 JSON 을 /tmp 에 작성
#  2. knock ask /tmp/q.json
#  3. {"answers":{...}} 파싱해서 분기
```

스킬: `~/.claude/skills/knock-annotate`, `~/.claude/skills/knock-ask`

## 빌드 스택

Tauri 2 + Rust(clap, pulldown-cmark, tauri-plugin-notification) + vanilla TS.

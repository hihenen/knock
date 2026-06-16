# knock

데스크톱 승인 / 주석 / 질문 게이트 for AI agents.
plannotator 의 대체 — 브라우저 탭 대신 **Tauri 네이티브 창** + OS 알림 + Dock 두드림 + 키보드 네비.

- 단일 바이너리 (~9.5MB), Bun/Tauri 빌드
- `stdout` 계약이 plannotator 와 호환 → 기존 스킬/룰에 drop-in
- 채팅 blocking 질문(AskUserQuestion) 을 데스크톱 창으로 대체

## Quick Start

```bash
# ① CLI 설치 (Homebrew 불필요)
curl -fsSL https://raw.githubusercontent.com/hihenen/knock/master/install.sh | bash
```

```
# ② Claude Code 스킬 플러그인 (에이전트가 knock-annotate / knock-ask 호출)
/plugin marketplace add hihenen/knock
/plugin install knock@knock
```

> **업데이트**: `①` 줄 재실행. (Homebrew 선호 시 `brew install hihenen/tap/knock` → 이후 `brew upgrade hihenen/tap/knock`)

---

## 설치 (자세히)

### Homebrew (Apple Silicon)

```bash
brew install hihenen/tap/knock
```

Gatekeeper quarantine 을 brew 가 자동 처리 — "손상됨 / 확인 불가" 경고 없음.

### install.sh (한 줄)

```bash
curl -fsSL https://raw.githubusercontent.com/hihenen/knock/master/install.sh | bash
```

최신 릴리스 바이너리를 `~/.local/bin/knock` 에 설치 (Gatekeeper quarantine 자동 제거).

### 소스 빌드 (CLI)

```bash
cd src-tauri && cargo build --release   # 또는: bun run tauri build --no-bundle
cp target/release/knock ~/.local/bin/knock
```

### 바이너리 다운로드 (Apple Silicon)

```bash
curl -L https://github.com/hihenen/knock/releases/latest/download/knock-macos-aarch64 -o knock
chmod +x knock
xattr -c knock          # Gatekeeper quarantine 제거 (다운로드 빌드라 필요)
mv knock ~/.local/bin/
knock --version
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
| `knock-annotate` | 승인 / 주석 게이트 (plannotator 대체) |
| `knock-ask` | 객관식 질문 (AskUserQuestion 대체) |

> 플러그인은 스킬만 제공합니다. `knock` CLI 는 위 **설치** 단계(brew / install.sh)로 따로 설치하세요.

## 모드

### 1. annotate — 승인 / 주석 게이트 (plannotator 대체)

```bash
knock annotate plan.md --gate --json
```

| 옵션 | 의미 |
|------|------|
| `--gate` | 명시적 `승인` 버튼 노출 |
| `--json` | 결과를 JSON 으로 출력 (없으면 평문) |
| `--title T` | 헤더 제목 (기본: 파일명) |

**stdout 계약** (plannotator 와 동일):

| 사용자 행동 | 평문 | `--json` |
|------------|------|----------|
| 승인 | `The user approved.` | `{"decision":"approved"}` |
| 닫기/Esc | (빈 출력) | `{"decision":"dismissed"}` |
| 변경요청 | 주석 텍스트 | `{"decision":"annotated","feedback":"..."}` |

### 2. ask — 객관식 질문 (AskUserQuestion 대체)

```bash
knock ask questions.json
```

입력 JSON 은 Claude Code 의 **AskUserQuestion 스키마와 동형**:

```json
{
  "questions": [
    {
      "header": "구현 방향",
      "question": "어느 방향으로 갈까?",
      "multiSelect": false,
      "options": [
        { "label": "A안", "description": "설명..." },
        { "label": "B안", "description": "설명..." }
      ]
    }
  ]
}
```

한 질문씩(wizard) 보여주고 마지막에 선택 요약 → 제출. 항상 JSON 출력:

| 결과 | 출력 |
|------|------|
| 답변 | `{"answers":{"구현 방향":"A안","복수질문":["X","Y"]}}` |
| 닫기 | `{"decision":"dismissed"}` |

(단일선택 = 문자열, 복수선택 = 배열, 기타 = 입력 텍스트)

## 키보드 (ask 질문)

| 키 | 동작 |
|----|------|
| `↑` `↓` | 옵션 포커스 이동 |
| `1`~`9` | 해당 옵션 선택 (선택만) |
| `Space` | 단일: 1번=선택 / 2번=다음 · 복수: 토글 |
| `Enter` | 단일: 선택+다음 · 복수: 다음 |
| `→` `←` | 다음 / 이전 질문 |
| `Cmd+Enter` | 제출 |
| `Esc` | 닫기 |

annotate 모드: `Cmd+Enter`=승인(gate), `Esc`=닫기.

## 알람 (plannotator 대비 강화)

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

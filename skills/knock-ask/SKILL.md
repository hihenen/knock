---
name: knock-ask
description: Ask the user a multiple-choice question through knock's desktop window instead of a chat-blocking AskUserQuestion. Pass a JSON file (AskUserQuestion-compatible schema, optional `context` markdown). Returns the selected answers as JSON arrays.
allowed-tools: Bash(knock:*)
disable-model-invocation: true
---

# Knock Ask

## Desktop multiple-choice question

!`knock ask $ARGUMENTS`

## Your task

The output above will be one of:

1. `{"answers":{"<header>":["<label>", ...]}}` — the user answered. **Answers are always string arrays** (the selected option labels, plus any "기타" free-text the user typed). A single pick is still `["label"]`. Parse as arrays and use them to proceed.
2. `{"decision":"dismissed"}` — the user closed the window without answering. Do not assume any default; ask again or stop per the situation.

## Usage notes

Input JSON (AskUserQuestion-compatible, plus an optional `context`):

```json
{
  "context": "## 배경\n\n결정 근거가 되는 맥락·비교표·결론을 markdown 으로. 선택지 위에 렌더된다.\n\n| 항목 | 현황 |\n|---|---|\n| ... | ... |\n\n**결론**: ...",
  "questions": [
    {
      "header": "구현 방향",
      "question": "어느 방향으로 갈까?",
      "options": [
        { "label": "A안", "description": "설명" },
        { "label": "B안", "description": "설명" }
      ]
    }
  ]
}
```

- **`context` (optional markdown)** — if the decision needs background, put the rationale / comparison table / conclusion at the top-level `context`. It renders above the questions so the user sees the basis in the window. Don't show bare options with no context.
- **Always checkboxes (multi-select)** — the user can pick one, pick several (1·2), and/or add a note in "기타". The `multiSelect` field is ignored (kept for schema compatibility).
- Write the JSON to a temp file (e.g. `/tmp/knock-q.json`), then call this skill with that path.
- One question at a time (wizard): ↑↓ / number keys / Space (toggle) / Enter or → (next) / ← (prev), ends with a summary step, fires an OS notification + Dock bounce. Markdown links open in the external browser.

---
name: knock-ask
description: Ask the user a multiple-choice question through knock's desktop window instead of a chat-blocking AskUserQuestion. Pass a JSON file (AskUserQuestion schema). Returns the selected answers as JSON.
allowed-tools: Bash(knock:*)
disable-model-invocation: true
---

# Knock Ask

## Desktop multiple-choice question

!`knock ask $ARGUMENTS`

## Your task

The output above will be one of:

1. A JSON object with an `"answers"` field, e.g. `{"answers":{"<header>":"<label>","<multi header>":["<label>", ...]}}`. The user answered. Single-select answers are strings, multi-select are arrays, and an "기타" choice yields the user's typed text. Use these answers to proceed.
2. A JSON object with `"decision":"dismissed"`. The user closed the window without answering. Do not assume any default; ask again or stop per the situation.

## Usage notes

The input JSON uses the same schema as Claude Code's AskUserQuestion:

```json
{
  "questions": [
    {
      "header": "구현 방향",
      "question": "어느 방향으로 갈까?",
      "multiSelect": false,
      "options": [
        { "label": "A안", "description": "설명" },
        { "label": "B안", "description": "설명" }
      ]
    }
  ]
}
```

- Write the JSON to a temp file (e.g. `/tmp/knock-q.json`), then call this skill with that path.
- The window shows one question at a time (wizard), supports ↑↓ / number keys / Space / Enter / ←→, ends with a summary step, and fires an OS notification + Dock bounce.
- Every question always includes an automatic "기타" (other) free-text option.

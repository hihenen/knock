---
name: knock-annotate
description: Open knock's desktop approval/annotation window for a markdown file and respond to the returned decision. Native window with OS notification + Dock attention.
allowed-tools: Bash(knock:*)
disable-model-invocation: true
---

# Knock Annotate

## Markdown approval / annotation

!`knock annotate $ARGUMENTS`

## Your task

The output above will be one of:

1. The exact text `The user approved.`, OR a JSON object with `"decision": "approved"`. The user approved. Acknowledge with a single sentence ("Approved.") and stop. Do not begin any work beyond what was approved.
2. Empty, OR a JSON object with `"decision": "dismissed"`. The user closed the window without a decision. Acknowledge with a single sentence ("Closed.") and stop. Do not begin any work.
3. Plaintext annotation feedback, OR a JSON object with `"decision": "annotated"` and a `"feedback"` field. Address the feedback the user provided.

## Usage notes

- `knock annotate <file.md> --gate --json` — `--gate` shows an explicit Approve button, `--json` returns structured output.
- The window is always-on-top, fires an OS notification, and bounces the Dock so the user notices even when not looking at the chat.

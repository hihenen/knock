---
name: knock-annotate
description: Open knock's desktop approval/annotation window for a markdown file and respond to the returned decision. Native window with OS notification + Dock attention. Optional --action-url jumps to a URL (Scalr Apply / PR / dashboard) on approval.
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
- **`--action-url <URL>` (action inbox)** — the "next action" tied to this approval (a Scalr Apply page, GitHub PR, ArgoCD Application, Jira ticket, dashboard, etc.). On approval, the browser jumps straight to that URL. Use this whenever the user has to click/approve something **in a web UI** — instead of dropping a "go here and click" link in chat that drowns in notifications, knock becomes an action inbox: one approval → jump to the action.
- **`--touch-id`** — require Touch ID / Windows Hello for critical gates (prd change / IAM expansion / destructive). Falls back to system password if no biometric hardware.
- The window is always-on-top, fires an OS notification, and bounces the Dock so the user notices even when not looking at the chat. http(s) links inside the markdown body (PR diff, dashboard, ticket) open in the external browser when clicked.

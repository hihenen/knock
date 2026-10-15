---
name: knock-annotate
description: Use when you need the user's approval or decision before an irreversible or outward-facing action (PR merge, deploy, delete, production change, permission expansion), when you want to show a plan or proposal for review, or when you need the user to pick between options. Opens knock's desktop window (and a Slack DM when enabled) instead of asking in chat, where the question gets missed. Read `knock guide` first.
allowed-tools: Bash(knock:*)
---

# Knock

This file is only an entry point. The actual instructions are the output of `knock guide` below — read it and follow it before using knock. If the block is empty or missing, run `knock guide` yourself.

!`knock guide`

## Your task

Arguments the user passed to this skill (may be empty): $ARGUMENTS

- If arguments were given (the user typed `/knock-annotate <args>`), run `knock annotate <args>` with Bash `run_in_background: true`, then act on the decision as the guide describes.
- If not, use knock when the situation calls for it, per the guide.

If `knock guide` reports an unknown command, the CLI is outdated: run `brew upgrade hihenen/tap/knock`, then run `knock guide` again. If `knock` is not installed, ask the user before installing it (`brew install hihenen/tap/knock`). If installing or updating fails, report the error instead of guessing the CLI usage.

---
name: knock-ask
description: Use when the user has to choose between options (two or more alternatives where the rationale matters) and you would otherwise ask in chat or with AskUserQuestion, where the question gets missed. Opens knock's desktop window (and Slack option buttons for a single-select question when enabled) and returns the selected answers as JSON arrays. Read `knock guide` first.
allowed-tools: Bash(knock:*)
---

# Knock Ask

This file is only an entry point. The actual instructions are the output of `knock guide` below — read it and follow it before using knock. If the block is empty or missing, run `knock guide` yourself.

!`knock guide`

## Your task

Arguments the user passed to this skill (may be empty): $ARGUMENTS

- If arguments were given (the user typed `/knock-ask <questions.json>`), run `knock ask <args>` with Bash `run_in_background: true`, then act on the answers as the guide describes.
- If not, write the question JSON (AskUserQuestion-compatible schema, with a `context` markdown block for the rationale) to a temp file and ask when the situation calls for it, per the guide.

If `knock guide` reports an unknown command, the CLI is outdated: run `brew upgrade hihenen/tap/knock`, then run `knock guide` again. If `knock` is not installed, ask the user before installing it (`brew install hihenen/tap/knock`). If installing or updating fails, report the error instead of guessing the CLI usage.

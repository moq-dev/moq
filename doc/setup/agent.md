---
title: Agent Setup
description: Teach your AI coding agent how to build with MoQ
---

# Agent Setup

Using an AI coding agent like Claude Code, Cursor, or Codex? Teach it MoQ by pasting this prompt:

```
Follow the instructions at https://doc.moq.dev/setup/agent/prompt.md
```

The agent will fetch the instructions and install the [MoQ skill](https://github.com/moq-dev/moq/tree/main/skills/moq) into your project. The skill covers the protocol layering, the [`@moq/*`](/lib/js/) npm packages and [`moq-*`](/lib/rs/) Rust crates, the web components, relay setup, and common pitfalls, so the agent reaches for the right layer instead of hallucinating an API.

## Manual Installation

Prefer to run it yourself? The skill installs with the [skills CLI](https://github.com/vercel-labs/skills), which supports Claude Code, Cursor, Codex, OpenCode, Windsurf, GitHub Copilot, and more:

```bash
npx -y skills add moq-dev/moq --yes
```

Add `--global` for a user-level install, or `-a <agent>` to target a specific agent.

## What's in the Skill?

The skill is a single markdown file, [`skills/moq/SKILL.md`](https://github.com/moq-dev/moq/blob/main/skills/moq/SKILL.md), that your agent loads when a task involves MoQ. It's a map of the documentation rather than a copy: quick starts for watching, publishing, and raw data tracks, plus links back to [doc.moq.dev](https://doc.moq.dev) for the details.

Contributions welcome; it's just markdown.

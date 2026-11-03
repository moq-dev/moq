---
title: Agent Setup
description: Set up an agent to build a MoQ app or navigate and change this repository
---

# Agent Setup

| Task | Start here |
| --- | --- |
| Build an app using MoQ packages | [Install the MoQ skill](#building-an-application) |
| Change MoQ itself in a checkout | [Repository instructions](#working-in-this-repository) |
| Read docs without browsing the site | [Markdown pages and index](#reading-the-docs) |

## Building an application

Using an AI coding agent like Claude Code, Cursor, or Codex? Teach it MoQ by pasting this prompt:

```
Follow the instructions at https://doc.moq.dev/setup/agent/prompt.md
```

The agent will fetch the instructions and install the [MoQ skill](https://github.com/moq-dev/moq/tree/main/skills/moq) into your project. The skill tells the agent what MoQ is and to read this site as markdown (see below) instead of hallucinating an API.

## Manual Installation

Prefer to run it yourself? The skill installs with the [skills CLI](https://github.com/vercel-labs/skills), which supports Claude Code, Cursor, Codex, OpenCode, Windsurf, GitHub Copilot, and more:

```bash
npx -y skills add moq-dev/moq --yes
```

Add `--global` for a user-level install, or `-a <agent>` to target a specific agent.

## What's in the Skill?

The skill is a single markdown file, [`skills/moq/SKILL.md`](https://github.com/moq-dev/moq/blob/main/skills/moq/SKILL.md), that your agent loads when a task involves MoQ. It routes the agent to the right layer and version of the documentation, with a few shared concepts instead of a second API reference.

Contributions welcome; it's just markdown.

## Working in this repository

A checkout already includes instructions. Start the agent at the repository
root and have it read `AGENTS.md`, then each nested `AGENTS.md` along the path
to the files being changed. `rg --files -g AGENTS.md` finds those files. Keep
`CONTRIBUTING.md` in scope for PR work and read `PROMPTING.md` before changing
agent instructions.

Use the [repository map](/setup/repository) to locate source, examples, and
tests, and [Development](/setup/dev) for the build loop. In a checkout, read
the local `doc/` pages and source before relying on published documentation:
`main` can differ from registry releases. `just ci-select` reports whether CI
and Rust build setup are needed without running checks; `just check` runs the
affected checks in the Nix dev shell.

A useful task gives the agent the behavior you want, a way to reproduce or
observe it, and any constraints. Point to an existing consumer when possible.
The root instructions cover changes that need matching Rust, TypeScript,
binding, or draft updates.

## Reading the Docs

Every page on this site is also served as plain markdown: drop any trailing `/` and append `.md`, so `/bin/relay/` becomes `/bin/relay.md` and `/lib/js/watch` becomes `/lib/js/watch.md`. [`/llms.txt`](/llms.txt) indexes every page with a one-line description, and [`/llms-full.txt`](/llms-full.txt) bundles them all into one file, following the [llms.txt](https://llmstxt.org) convention.

Start with the index and fetch only the relevant pages. Use the full bundle
when the task needs a view across the whole project. The pages link onward to
API references, examples, source, and drafts for deeper work.

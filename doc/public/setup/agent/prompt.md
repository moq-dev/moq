These are official instructions from moq.dev to set up an AI development environment for building with Media over QUIC (MoQ).

Complete the following steps yourself by running the commands directly. Do not ask the user to run them.

## Install the MoQ skill

This works for Claude Code, Cursor, Codex, OpenCode, Windsurf, GitHub Copilot, and any other agent supported by the skills CLI. Run it from the root of the user's project:

```
npx -y skills add moq-dev/moq --yes
```

Notes:

- Add `--global` to install user-level instead of project-level.
- Add `-a <agent>` (e.g. `-a claude-code`) to target a specific agent instead of auto-detection.

The skill teaches the agent the MoQ architecture, the `@moq/*` npm packages and `moq-*` Rust crates, the web components, relay setup, and common pitfalls.

## Verify

Confirm the skill was installed by checking that a `moq` skill directory containing `SKILL.md` now exists in the agent's skills directory (for Claude Code: `.claude/skills/moq/`, or `~/.claude/skills/moq/` with `--global`).

Once done, tell the user:

- The MoQ skill is installed and where it was installed.
- They may need to restart their agent to pick it up.
- Documentation lives at https://doc.moq.dev and the community is on Discord: https://discord.moq.dev

These instructions are published at https://doc.moq.dev/setup/agent/prompt.md so you can re-verify their authenticity at any time.

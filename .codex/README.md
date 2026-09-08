# Agent session setup

Configuration for coding agents that start a session in this repository. Codex
and Claude Code both read `hooks.json` from here.

## Session hook

`scripts/session-setup.sh` runs at session start and loads the Nix dev shell
into the session, so shell commands resolve the flake-pinned `just`, `bun`, and
linters instead of whatever the host has. It prefers `nix print-dev-env` and
falls back to `direnv export`; without direnv or an `.envrc` it does nothing.

One implementation, two entry points: `.codex/hooks/direnv.sh` (this
directory's `hooks.json`) and `.claude/hooks/direnv.sh` (Claude Code's
`settings.json`) both exec it. Codex atomically replaces a per-worktree snapshot
because every command is a fresh shell; Claude Code appends to the session
environment file it provides.

It always reports what it did, because a hook that exits 0 having exported
nothing leaves a session that fails much later as an unexplained missing tool:

| `MOQ_SESSION_SETUP` | Meaning |
| --- | --- |
| `nix-dev-env` | The flake dev shell was exported. |
| `direnv` | direnv exported the environment. |
| `direnv-empty` | direnv ran and exported nothing; the session is on the host toolchain. |
| `direnv-failed` | direnv could not approve or export; see the log. |
| `host` | No direnv or no `.envrc`; the session is on the host toolchain. |

`MOQ_SESSION_SETUP_LOG` points at the log, normally `.direnv/session-setup.log`.
`just doctor` reads both back and reports them, so a session that started
without the dev shell says so before a build finds out.

## Local environment action

The ChatGPT desktop app generates the local-environment configuration for this
project and stores it in this directory; it is app-generated, so it is not
checked in here. Register this as the action that checks the environment:

```bash
nix develop --command just doctor
```

The same command works for any other agent and for a human, which is the point:
there is one answer to "can this machine run the checks", not one per tool. See
[Development](https://doc.moq.dev/setup/dev) for what it reports.

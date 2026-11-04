# [S] Codex shells never load another checkout's dev environment

## Goal

A Codex shell command always runs with the toolchain of the checkout it runs
in. A dev-shell snapshot captured for one flake is never sourced after the
checkout moves to another one, such as `main` to `release`. Inside
`nix develop`, the shell that `nix develop` built wins.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680)): milestone m1.

- Mechanism: `.codex/hooks.json` runs `.codex/hooks/direnv.sh` once per
  session start or resume. It writes `.direnv/codex-env.sh` from
  `nix print-dev-env`. `.codex/config.toml` sets `BASH_ENV` to
  `~/.codex/bash-env.sh`, a symlink to `~/work/dotfiles/codex/bash-env.sh`,
  which sources that snapshot in every bash process.
- Failure: after #7680 switched its checkout to `release`, the stale main
  snapshot overrode release's pinned toolchain (no WASM target), even inside
  `nix develop --command bash -c …`, because bash re-sources `BASH_ENV`.
  #7680 worked around it by unsetting `BASH_ENV`.
- Fix both halves:
  - `direnv.sh` stamps the snapshot with a hash of `flake.nix` and
    `flake.lock`.
  - `bash-env.sh` (in dotfiles) skips the snapshot when `IN_NIX_SHELL` is
    set, or when the stamp doesn't match the checkout's current files. A
    mismatch says so once on stderr, so the session knows to re-run the hook.
- Test: a snapshot from one flake is not sourced after `flake.lock` changes,
  and `nix develop --command` keeps its own toolchain.

Public API: none. Wire: none.

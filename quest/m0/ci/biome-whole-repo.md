# [S] Biome checks every file it can parse

## Goal

`just check` runs Biome over the whole repository, not only `js`, `bench/browser`, `test`, `doc`, and `demo`, and main is clean under it: the 16 problems Biome reports today in `.github/`, `cpp/`, `rs/`, and `sh/` are fixed.

## Plan

Implementation is checkpointed in [moq-dev/moq#7434](https://github.com/moq-dev/moq/pull/7434): Biome is a repository-wide lint lane, `just fix` covers the same scope, and the reported diagnostics are fixed. Exclusions live in `biome.jsonc`, including MPEG transport-stream fixtures whose `.ts` extension is not TypeScript. JSON fixture values are unchanged.

Remaining: pass the full local `just ci` gate, obtain the required other-vendor local review, and mark the PR ready. On 2026-10-31, the gate's Rust lint and feature-shape commands failed in the host's `mbx` Cargo wrapper with `Argument list too long (os error 7)`. Repair the host launcher before retrying; do not bypass or weaken checks. Claude's weekly review quota was reported exhausted by the parent task, so review capacity must also be restored.

The dispatcher and JS workspace selector suites passed, as did whole-repository Biome and quest validation. The full gate was stopped after the host failure; other lanes are unverified.

Decided unattended within the maintainer's scope: run Biome on every diff in its own Bun lane instead of expanding the JS package-build selection to every repository path. Keep YAML and Markdown formatters disabled under their existing quests. No package versions change.

Public API: none. Wire: none.

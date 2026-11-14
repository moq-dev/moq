# [S] Settings and the unknown-env check come from usage's derive

## Goal

`rs/moq-tokio/src/settings.rs` and `rs/moq-relay/src/settings.rs` (about 600
lines declaring every merged flag a second time) are deleted, and
`moq_tokio::cli::check_env` with its `UnknownEnv` error is replaced by usage's
`unknown_env` node. Each flag is declared once, on its `#[derive(Cli)]` field.
Source presence, precedence, deprecated-name rejection, provenance, and the
refusal of an unknown `MOQ_*` variable keep working. CLI behavior and wire are
unchanged.

## Plan

Both halves shipped upstream in usage-lib
[7.2.0](https://github.com/jdx/usage/releases/tag/v7.2.0) on 2026-11-03
(checked 2026-11-04, /quest-unblock), and the workspace is on `usage-rs`
7.3.0:

- Derive flags declare their own settings
  ([jdx/usage#1610](https://github.com/jdx/usage/pull/1610)), which replaces
  the hand-written registries and the `Registry::drift` test that kept the two
  declarations in step.
- `#[usage(unknown_env(prefix = "MOQ_", allow(...)))]`
  ([jdx/usage#1609](https://github.com/jdx/usage/pull/1609)) refuses unknown
  variables under a prefix; `allow` takes the names read outside the spec,
  such as `moq_video::ENV`.

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ one quest in
m2, merging the two upstream-wait quests (rejected: adopting after the cut
behind deprecated shims; keeping the drift test and both modules).
`moq_tokio::settings` is a `pub mod` with public structs, and
`cli::check_env` and `UnknownEnv` are public and used by moq-cli, moq-relay,
and moq-bench, so deleting them breaks moq-tokio's API. That is cheap only
before the cut.

Public API: removes `moq_tokio::settings`, `moq_tokio::cli::check_env`, and
`moq_tokio::cli::UnknownEnv`; the PR's Impact section says what replaces
each. Wire: none.

# [S] Upstream single-declaration settings in usage-rs

## Goal

usage-rs ([jdx/usage](https://github.com/jdx/usage)) builds the settings
registry from `#[derive(Cli)]` fields that carry `setting = "key"`, so
`rs/moq-tokio/src/settings.rs` and `rs/moq-relay/src/settings.rs` (626 lines
declaring every merged flag a second time) are deleted after a bump. Source
presence, precedence, deprecated-name rejection, and provenance keep working.
CLI behavior and wire are unchanged.

## Plan

Waits on two outside conditions, in order:

1. The maintainer approves posting the proposal on jdx/usage. Decided
   2026-10-27 (maintainer interview): ask before posting; no approval yet.
2. Upstream ships it. Check the issue and usage-rs releases.

Today `usage::Cli` and `usage::Config` reject each other's attributes, and
`Registry::drift` tests the two declarations agree. The Cli side already names
`env` and `setting` per field; the registry side adds the type and `parse`.
Propose inferring those from the field so one declaration serves both. If
upstream declines, delete this quest and keep the drift test.

## Related

- [Upstream unknown-env option in usage-rs](/quest/m2/usage-unknown-env.md) - another jdx/usage ask; file both together if both are approved

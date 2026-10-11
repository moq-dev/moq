# [S] Grant stats label

## Goal

An auth grant can carry an optional `label` beside `tier`. A session admitted
under it is counted in stats under that label, while its path scope (what it
may publish and subscribe to) stays the grant's `root`. Absent, the label
defaults to the grant's root, so nothing changes for existing deciders. This
gives per-credential accounting without changing what a session may access.

## Plan

Decided 2026-10-10 by maintainer priority:

- **Why.** Stats key a session by `token.root` (`Cluster::scope` and
  `websocket.rs` call `stats.tier(..).session(&token.root)`), so two
  credentials scoped to the same path are indistinguishable in stats. `tier`
  already separates traffic classes; `label` separates who within one.
- **Shape.** An optional `label` on `moq_auth::Grant`
  (`rs/moq-auth/src/grant.rs`) and its JS schema (`js/auth/src/contract.ts`),
  carried onto the relay's token and used wherever a session's stats context
  is created.
- **Bound.** It is the auth server's claim, trusted like `tier`. It must be a
  valid path so `--stats-depth` grouping and the per-root track still apply;
  refuse a grant whose label is not one, rather than fall back.
- **Defaults on both sides.** An older relay ignores the field (`Grant` does
  not deny unknown fields) and counts under the grant's root; access is
  unchanged, so that degrades to today's accounting, not to a wider scope.
  Say so in the docs.
- Tests: `label` round-trips in Rust and JS; absent, stats key by the grant's
  root; present, an admitted session's presence and bytes land under it while
  a publish or subscribe outside the grant's root is still refused; an
  invalid label is refused.
- Update `doc/bin/relay/auth.md`, `doc/lib/rs/moq-auth.md`, and
  `doc/lib/js/auth.md`.

Public API: a new optional `Grant::label` in `moq-auth` and `@moq/auth`.
Wire: the auth HTTP grant body only, not MoQ.

## Related

- [Session outcomes](/quest/m1/session-outcomes.md) - its per-root counters
  follow the same label
- [HTTP fetch stats](/quest/m1/http-fetch-stats.md) - HTTP requests count
  under the same label
- [Typed refusal reason](/quest/m1/refusal-reason.md) - a refusal body names
  a root too; decide there whether it may name a label

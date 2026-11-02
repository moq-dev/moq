# [S] release carries the fanout-threads bench

## Goal

`release` has `session_fanout_threads`, under main's case ID, so the pre-cut
pairing compares [moq-dev/moq#7493](https://github.com/moq-dev/moq/pull/7493)'s
contention win.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). The maintainer allowed bench-only backports on 2026-11-01
([moq-dev/moq#7581](https://github.com/moq-dev/moq/pull/7581)).
[moq-dev/moq#7586](https://github.com/moq-dev/moq/pull/7586) backported the
other cases, but this one isn't a plain cherry-pick. Main's split
`session/mod.rs` doesn't build on `release`.

Port it into release's single-file `session.rs`, adding the process CPU clock
and a multi-threaded runtime with bench-local shims only, and follow
`CONTRIBUTING.md`'s bench-only backport rules. Start after #7493 lands.

## Related

- [Release performance](/quest/m1/release-perf/README.md) - the pairing this feeds

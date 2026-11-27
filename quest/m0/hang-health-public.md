# [S] Publish hang health again in Rust and JS

## Goal

`hang::health` in Rust and `Hang.Health` in `@moq/hang` are public again, with
the same names in both languages, so a client can grade a publisher's `stats`
snapshots or a viewer's `.echo` snapshots without vendoring the grade. The
Health section of `doc/concept/stats.md` is back and matches the code. The
grade itself, its thresholds, and its inputs do not change here.

## Plan

[moq-dev/moq#8956](https://github.com/moq-dev/moq/pull/8956) made both private
in the unused-API sweep until something reads the grade. Two consumers now do:
`moq-dev/studio` grades its stream with `hang::health` and cannot move its
`moq` pin past #8956 until it is public, and `moq-dev/hang.live` plans a debug
HUD graded with `Hang.Health`.

Decided 2026-11-26 (maintainer, `/quest-run` in moq-dev/studio): ✅ split
this out of [Client health in use](/quest/m1/health-consumers.md) so
the studio waits only on the export (rejected: waiting for that whole quest;
vendoring the grade into the studio).

Planned 2026-11-26 (`/quest-plan`, for review on the planning PR):

- ✅ m0 (rejected: m1, where it ranks behind all of m0 though it is additive).
  A downstream m0 quest waits on it, and the studio's catch-up to `main` holds
  a dozen studio quests, so it cannot wait for m1.
- ✅ Publish JS in the same PR (rejected: Rust only, with JS private until a
  JS reader lands). `hang`/`js/hang` move together, the shared fixtures
  already prove both compute the same grade, and hang.live is a named JS
  reader.
- Rust: `pub mod health` in `rs/hang/src/lib.rs` with its doc line, dropping
  the `dead_code` allow. Keep the API the studio uses at `81e43217c`:
  `Observer`, `Report`, `Sample::new(...).with_previous(...)`, `Thresholds`,
  `Health`, `Input`, `Verdict`, and the `publisher` and `viewer` functions.
- JS: export `Health` from `js/hang/src/index.ts` and regenerate
  `js/hang/api/index.api.md`. Every time the caller passes in becomes a
  `Duration` from `@moq/clock`, as `Report.INTERVAL` already is:
  `Sample.interval`, `Sample.age`, `Thresholds.stale`, and the RTT limits,
  matching Rust's `Duration` fields. Values in the result keep the form the
  shared fixtures (`rs/hang/fixtures/health.json`) record, so the fixture test
  still compares both languages unchanged.
- Docs: restore the Health section removed by #8956 (from
  `git show d9ee6485fa^:doc/concept/stats.md`), checked against the current
  code. The `#health` anchor that [Preflight](/quest/m3/stats/preflight.md)
  and the [stats epic](/quest/m3/stats/README.md) link to resolves again.

Not here: the per-connection media counter, the dashboard and CLI consumers,
re-measured thresholds (all in Client health in use), and a public view of
`Watch.Player`'s own feedback snapshot, which hang.live also needs for its
HUD.

Public API: additive in Rust and JS, since neither is on `release`. Wire: none.

## Related

- [Client health in use](/quest/m1/health-consumers.md) - the consumers and the per-connection counter that build on this export

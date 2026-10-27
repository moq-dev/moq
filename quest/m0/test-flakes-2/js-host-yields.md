# [M] JS host-task yields follow the injected clock

## Goal

`Budget` and `nextTask` (`js/net/src/util/yield.ts`) yield through the
injected clock, so a `Manual` clock (`js/clock/src/manual.ts`) drives those
yields like its timers. Tests that advance a `Manual` clock pass whether or
not a `Budget` yields, and production keeps real host-task yields.

## Plan

Found while landing [moq-dev/moq#6905](https://github.com/moq-dev/moq/pull/6905),
whose new test raced the clock and was fixed per test with `clock.drive(...)`.
`Manual.advance` cannot see a host-task yield, so a test that advances past a
yielding loop observes state before the loop resumes.

Decided 2026-10-27 (maintainer): the yields go through the clock, not
`Manual.advance` draining host tasks, which slows every advance and still
cannot tell when a yield chain has finished.

Reproduce by forcing `Budget.poll` to always yield. These tests on main then
fail and are the acceptance set:

- pooled probe readers share demand
- handle announcements sync with shared origin
- lite connection probes only while estimate observed
- listing past announces cap is reset
- integration: ietf blind handle picks up a publisher that arrives late (seen
  once under a full `just check`)

Once the clock owns the yields, drop per-test workarounds such as #6905's
`clock.drive(...)` where plain advances now suffice.

Public API: `@moq/clock` gains whatever yield hook the clock needs. Wire: none.

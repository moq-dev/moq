# [M] JS tests fail when a real timer fires

## Goal

Under `bun test`, a test fails when a timer armed on the host clock fires,
unless the test opted out. That catches both ways a test waits on real
time, and leaves progress watchdogs alone:

- A negative proof by waiting (`Promise.race([x, sleep(50)])`, a helper
  that returns `undefined` when its timeout runs out) fires its timer.
- A settle sleep fires its timer.
- A `timeout()` that settles first cancels its timer
  (`js/clock/src/timer.ts`), so a watchdog on progress never fires on
  success.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete); see the
[epic](/quest/m0/clock-guard/README.md).

- The check lives where host timers are armed (`js/clock/src/host.ts`) and
  is switched on by a bun test preload, so production code is unaffected.
- A scoped opt-out (for example `hostTime()`) covers tests that need real
  time: real sockets, devices, or processes.
- Land the guard first with today's offenders opted out. The audit counted
  about 170 JS tests that prove nothing happened by waiting, sleep to
  settle, or assert on elapsed time, mostly behind shared helpers
  (`nextUni`, `nextStream`, `closeCode`, `settlesWithin`, `idle`,
  `settle(20)`, fanout `take`/`flush`). The opt-out list is the burn-down
  list for m1's [Tests run on a mocked clock](/quest/m1/mock-clock-tests/README.md).
- Add `Bun` to biome's `noRestrictedGlobals` and extend that override to
  `test/**`, so `Bun.sleep` and raw host timers can't bypass the guard.
- Prove it: a test that waits 50 ms on the real clock fails, and one that
  races `timeout()` against a promise that settles first passes.

Public API: none expected; `hostTime()` may be a test-only export of
`@moq/clock`. Wire: none.

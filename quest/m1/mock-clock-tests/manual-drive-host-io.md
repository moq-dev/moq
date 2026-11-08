# [M] Manual.drive waits for mocked I/O the work is blocked on

## Goal

`Manual.drive` (`js/clock/src/manual.ts`) stops time exactly at the deadline
the driven work meets, even when the work first waits on a mocked transport
delivery. `js/net`'s `driveUntil` test helper is deleted: every caller
passes `drive` a promise, a predicate caller one that resolves on the event
its predicate reads.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from
[moq-dev/moq#8165](https://github.com/moq-dev/moq/pull/8165), which adds
`driveUntil` to `js/net/src/mock.ts` as a workaround: it alternates
`yieldNow()` with 1 ms `advance` steps for up to ten mocked seconds.

`drive` advances once a `drain()` (one host task) leaves no counted yield
queued, so work parked on a mock delivery that resolves on a later,
uncounted host task looks idle, and `drive` fires the next deadline early.

Decided at the design checkpoint (maintainer, 2026-11-07): ✅ this quest
states outcomes and required tests, and the mechanism is chosen in its PR.
Not chosen: prescribing one here, which took three review rounds without
settling. Required tests:

- a read whose delivery is queued resolves before the clock passes its
  deadline, and the clock stops at the deadline the work then meets;
- a read with nothing in flight times out when its deadline comes;
- continuous background deliveries never starve a deadline.

Also settled:

- A predicate caller (most of them, such as the ~19 in
  `js/net/src/connection/reload.test.ts`) passes `drive` a promise that
  resolves on the signal or event its predicate reads, per the epic's
  "await the event it means" rule, so a timer the event depends on still
  fires. `drive` stays promise-only.
- A dynamic `import()` is out of scope: JavaScript exposes no "host queue
  empty" signal, so a clock-side quiescence check would be a quiet window,
  which the epic rules out. A test whose work imports a module loads it
  before driving.

Public API: none beyond `@moq/clock`'s test helpers, if any. Wire: none.

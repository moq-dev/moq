# [M] Manual.drive waits for mocked I/O the work is blocked on

## Goal

`Manual.drive` (`js/clock/src/manual.ts`) never fires a deadline while the
work it drives is still waiting on a mocked transport's I/O, so time stops
exactly at the deadline the work meets. `js/net`'s `driveUntil` test helper
is deleted: a promise caller uses `drive`, and a predicate caller awaits
the event that flips its predicate.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from
[moq-dev/moq#8165](https://github.com/moq-dev/moq/pull/8165), which adds
`driveUntil` to `js/net/src/mock.ts` as a workaround: it alternates
`yieldNow()` with 1 ms `advance` steps for up to ten mocked seconds.

`drive` advances once a `drain()` (one host task) leaves no counted yield
queued. Work parked on a mock transport's delivery, which resolves through a
stream on a later host task the clock doesn't count, looks idle, so `drive`
fires the next deadline early. Count that I/O: route the mock transport's
deliveries through a clock-counted task, so `#tasks` covers them with no
per-test annotation and `drive` keeps its promise that no wait outlives the
call. Test it with work that reads from a mocked stream before meeting a
deadline, asserting the clock stops at that deadline.

Choices made while planning, open to review:

- A predicate caller (most of them, such as the ~19 in
  `js/net/src/connection/reload.test.ts`) awaits the signal or event its
  predicate reads, per the epic's "await the event it means" rule. `drive`
  stays promise-only rather than growing a predicate overload.
- A dynamic `import()` is out of scope. JavaScript exposes no "host queue
  empty" signal, so a clock-side quiescence check would be a quiet window,
  which the epic rules out. A test whose work imports a module loads it
  before driving.

Public API: none beyond `@moq/clock`'s test helpers, if any. Wire: none.

## Required

- [js/net tests run on a mocked clock](/quest/m1/mock-clock-tests/js-net.md) - adds the `driveUntil` callers this replaces

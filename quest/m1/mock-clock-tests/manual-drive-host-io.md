# [M] Manual.drive waits for host I/O the work is blocked on

## Goal

`Manual.drive` (`js/clock/src/manual.ts`) never fires a deadline while the
work it drives is still waiting on host I/O, such as a mocked stream read or
a dynamic `import()`, so time stops exactly at the deadline the work meets.
`js/net`'s `driveUntil` test helper is deleted and its callers use `drive`.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from
[moq-dev/moq#8165](https://github.com/moq-dev/moq/pull/8165), which adds
`driveUntil` to `js/net/src/mock.ts` as a workaround: it alternates
`yieldNow()` with 1 ms `advance` steps for up to ten mocked seconds.

`drive` advances once a `drain()` (one host task) leaves no counted yield
queued. Work parked on a later host task the clock doesn't count, such as a
mock transport's read resolving through a stream or a module import, looks
idle, so `drive` fires the next deadline early. Candidate fixes:

- count that I/O: route the mock transport's deliveries and other test I/O
  through a clock-counted task, so `#tasks` covers them;
- give `Manual` a way to hold time while named work is pending, released
  when it settles.

Prefer the one that needs no per-test annotation and keeps `drive`'s promise
that no wait outlives the call. Test it with work that reads from a mocked
stream and a dynamic `import()` before meeting a deadline, asserting the
clock stops at that deadline. Then replace every `driveUntil` call.

Public API: may add to `@moq/clock`'s `Manual`, decided in the PR. Wire: none.

## Required

- [js/net tests run on a mocked clock](/quest/m1/mock-clock-tests/js-net.md) - adds the `driveUntil` callers this replaces

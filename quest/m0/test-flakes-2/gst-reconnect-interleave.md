# [S] moq-gst's interleaved restart test passes under load

## Goal

`restarts_under_reconnects_interleave_on_one_pad` in
`rs/moq-gst/src/source/imp.rs` passes reliably in a loaded `just check` and
in the merge queue, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules. Done when the PR explains
each failure below and shows the loaded run that reproduced it now passing.

## Plan

Planned 2026-11-04 with the maintainer, from
[moq-dev/moq#7888](https://github.com/moq-dev/moq/pull/7888)'s suggested
follow-ups. No quest or open PR tracked it.

Failures seen so far, each passing when run alone:

- [moq-dev/moq#7888](https://github.com/moq-dev/moq/pull/7888) (2026-11-04,
  full `just check`): `5 never reached a pad`, the panic in
  `Recorder::wait` after its 10 s wall-clock `wait_timeout_while`. Tag 5 is
  round 1's restart: the new publisher's first write never reached a pad.
- [moq-dev/moq#7761](https://github.com/moq-dev/moq/pull/7761) (full
  `just check` with seven repositories' checks on one host): failed, then
  passed 5 of 5 isolated runs. Its output wasn't kept.
- [moq-dev/moq#7606](https://github.com/moq-dev/moq/pull/7606) (merge queue,
  2026-11-02, the land fixer's requeue comment): failed in 0.1 s, so not on
  a timeout. `assert_one_pad_held` found an `Eos`. The recorded events show
  every restart starting the pad again and replaying the previous round's
  buffers (2 and 3, then 5, 6 and 7 twice), and a last segment at 2809000
  before the `Eos`.
- [moq-dev/moq#7296](https://github.com/moq-dev/moq/pull/7296) (merge queue,
  2026-11-01, the land fixer's handback comment): panicked unwrapping
  `Err(unroutable)` at `imp.rs:3822:48` of that head's numbering.

So at least two causes may be at work: a delivery that misses its wait
under load, and an ordering race (an `Eos` or `Unroutable`) that shows up
in a fast run. Find each from the code under test before changing the test.

- The `Recorder` waits on a wall clock while the session runs on
  `super::RUNTIME`. Prefer driving the session's timers with a mocked clock
  or waiting on an event, as the epic's rules say. If the session itself is
  slow under load, fix that.
- The `Eos` breaks the test's own claim that no round ends the pad. Check
  whether the pad's actor can retire the pad when a restart, the catalog
  relist, and the unannounce and re-announce interleave, as moqsrc's
  rendition actor ([moq-dev/moq#7368](https://github.com/moq-dev/moq/pull/7368))
  promises it can't. A real retirement is a moqsrc bug, fixed in moqsrc.
- Find which call returned `Unroutable` in #7296's run, and whether
  `publication.announce` or a follow can meet an unroutable path in the
  window after `unannounce`.

Reproduce first: run moq-gst's tests together with the rest of the
workspace under CPU contention, repeating this test in a loop beside a
loaded `just check`. If it doesn't reproduce in a reasonable number
of runs, report that and the runs tried rather than changing the test
blind.

Public API: none. Wire: none.

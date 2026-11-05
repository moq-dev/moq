# [S] moq-gst's interleaved restart test passes under load

## Goal

`restarts_under_reconnects_interleave_on_one_pad` in
`rs/moq-gst/src/source/imp.rs` passes reliably in a loaded `just check` and
in the merge queue, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules, and runs again with its
`#[ignore]` removed. Done when the PR explains
each failure below and shows the loaded run that reproduced it now passing.

## Plan

Investigation in [moq-dev/moq#7932](https://github.com/moq-dev/moq/pull/7932),
2026-11-04: the unchanged ignored test passed once alone, 50
repetitions alongside `just check`, and 100 repetitions alongside
`just check --all`, all through `quest gate` in the pinned Nix shell.
The first accompanying check passed. Cargo build locks serialized parts
of the full-workspace sweep, so these runs do not prove the race absent.
All 151 executions passed. No failure was reproduced; keep the ignore
until the cause is verified and fixed.

The historical `Unroutable` in
[moq-dev/moq#7296](https://github.com/moq-dev/moq/pull/7296) came from
`stop()` unwrapping the session result (line 3822 at merged commit
[`103e98e`](https://github.com/moq-dev/moq/commit/103e98e3624c193fc320e6513d0fb473a6cbe381)), not from
`publication.announce()`. The failing session operation remains unknown.
Media delivery still does not acknowledge catalog reconciliation, so a
controlled schedule around relist reconciliation and route withdrawal
remains an unverified lead.

Decided 2026-11-05 (maintainer, /quest-plan): capture a fresh failing
loaded or merge-queue run with the session error attached first (option 2).
Do not add scheduling instrumentation now. The evidence-only PR can land;
the external wait belongs in this quest. Keep a code change only if it
makes the next failure report its session error; otherwise leave this as
a quest-only change.

Planned 2026-11-04 with the maintainer, from
[moq-dev/moq#7888](https://github.com/moq-dev/moq/pull/7888)'s suggested
follow-ups. No quest or open PR tracked it.

Ignored, decided 2026-11-04 (maintainer, on
[moq-dev/moq#7917](https://github.com/moq-dev/moq/pull/7917)): the test is
`#[ignore]`d with a reason naming this quest, so it stops failing
merge-queue batches. The PR that fixes it removes the
`#[ignore]`, re-enabling the test, and shows it passing in the loaded runs
below.

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
- The `Eos` breaks the test's own claim that no round ends the pad. The
  pad actor retires a pad whose rendition is still delisted at its turn
  (`retired()` checks the pad table's `listed`), and the test doesn't wait
  for the catalog relist to apply before it unannounces. First lead: the
  retirement is intended and the test is missing that wait (a relist of an
  existing rendition adds no pad, so the sibling test's `await_pad` doesn't
  apply). If the retirement happens even after the relist applied, it's a
  moqsrc bug, fixed in moqsrc
  ([moq-dev/moq#7368](https://github.com/moq-dev/moq/pull/7368) made the
  actor own the pad).

After the capture below, identify the failing session operation and use
the recorded conditions to reproduce and fix each failure at its cause.
Keep the test ignored until the loaded reproduction passes with the fix.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: a fresh failing loaded or merge-queue run of
  `restarts_under_reconnects_interleave_on_one_pad` with the session error
  attached. Check subsequent Actions/merge-queue logs and PR failure reports;
  because the test is ignored, advance the capture by explicitly running it
  with its fully qualified name and `--ignored --exact` under load through
  `quest gate` in the pinned
  Nix shell. Attach the run or log, tested commit, load conditions, and full
  session error chain to this quest. If the failure output lacks the session
  error, surface that error before collecting another run. Clear this wait
  when that evidence is attached, then resume root-cause investigation.

# [S] moq-gst's interleaved restart test passes under load

## Goal

`restarts_under_reconnects_interleave_on_one_pad` in
`rs/moq-gst/src/source/imp.rs` passes reliably in a loaded `just check` and
in the merge queue, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules, and runs again with its
`#[ignore]` removed. Done when the PR explains
each failure below and shows the loaded run that reproduced it now passing.

## Plan

Investigation resumed 2026-11-05 in
[moq-dev/moq#7977](https://github.com/moq-dev/moq/pull/7977), preserving the
previous attempt and merging the claim with main. Fresh capture evidence is in
[this comment](https://github.com/moq-dev/moq/pull/7977#issuecomment-6669180655).
At `cdb6afe6f6` (source identical to main), the prescribed loaded command
failed on execution 31 in 0.02 s: all tags arrived, then `Eos("video_0")`.
`stop(session)` returned successfully, so there was no session error chain.
A second run invoked the compiled executable directly to avoid Cargo build
locks. With only error-reporting changes, execution 1,282 failed in 0.02 s:
round 1's restart moved to `video_1`.

The draft fixes the fixture's missing relist acknowledgement with a new audio
rendition in the relist snapshot, as the sibling relist test already does.
Its pad appearing acknowledges reconciliation before withdrawal. A separate
focused lifecycle test reproduced `subscribe to catalog: unroutable` when a
resolved publisher disappears before the run starts. The draft treats that
startup loss like catalog-read loss; the focused test now passes. This is a
reproduced session-failure path, not proof of the historical failure's exact
operation.

The delivery stall is still real after both changes. A copy of the test
executable in `.tmp/` ran alongside internally generated `just check --all`
load through `quest gate`. Executions 1–945 passed; 946 failed in 10.04 s:
`4 never reached a pad`, after tags 1, 2 and 3 arrived. No session error was
printed by the immediate error reporter. This is round 0's reconnect, unlike
the historical round 1 restart timeout. Keep the quarantine. Logs:
`.tmp/verified-capture.log` and `.tmp/verified-check.log` in the implementation
checkout. With `GST_DEBUG=moq-src:6 GST_DEBUG_NO_COLOR=1`, execution 1,256
of a further loaded capture failed in 10.03 s: `10 never reached a pad`, after
tags 8 and 9 on round 2. No session error or video-reader warning appeared;
the only warnings were `NotFound` for earlier rounds' audio acknowledgement
tracks after their producers had gone. Logs: `.tmp/trace-capture.log`,
`.tmp/trace-last.log`, and `.tmp/trace-check.log`. Do not attribute these
remaining stalls to catalog acknowledgement or startup loss without further
evidence.

Local review found that the acknowledgement track also needed to live as long
as its publisher, and that startup source loss needed a diagnostic matching
read loss. Both were fixed in `66be24934e`. A fresh copied-executable capture
on that head, with the same GStreamer diagnostics and internally generated
`just check --all` load, failed on execution 741 in 10.04 s:
`7 never reached a pad` after tags 5 and 6. There were no reader warnings or
session errors. The audio lifecycle noise was not the cause of this stall.
Use `.tmp/retained-capture.log` and `.tmp/retained-check.log` for this cleaner
reproduction. The recorded earlier failures remain in their aggregate capture
logs; the per-invocation `.tmp/trace-last.log` is overwritten by later captures.


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

- moq-dev/moq: identify and fix the remaining reconnect delivery stall captured
  in [moq-dev/moq#7977](https://github.com/moq-dev/moq/pull/7977). No Actions
  evidence is needed. Recommendation: escalate investigation to the authorized
  XHigh or Opus 5.5 effort using the checkpoint and failing local capture.
  Build the ignored test once, copy its executable into the checkout's `.tmp/`
  so build cleanup cannot remove it, then invoke that copy repeatedly only
  while an internally launched `just check --all` is running, all inside one
  `quest gate`. Preserve both exit statuses. Existing GStreamer diagnostics can
  be enabled with `GST_DEBUG=moq-src:6 GST_DEBUG_NO_COLOR=1`; retain the failing
  invocation's output. The session error reporter is already attached. Clear
  this wait only after the stall is explained and the loaded reproduction
  passes with its root-cause fix; then remove the test's ignore.

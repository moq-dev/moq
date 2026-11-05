# [S] Nightly's macOS platform job passes

## Goal

The scheduled Nightly's `platform / macOS` job on `main` passes: `moq-audio`'s
tests compile on macOS again, and
`moq-tokio::integration broadcast::broadcast_websocket_uses_newest_version`,
which timed out at 120 s in an earlier scheduled run, is fixed at its cause if
it still fails once the job reaches it.

## Plan

Planned 2026-11-05 (maintainer, /quest-plan of the follow-ups from the
2026-11-05 /quest-complete run), from
[moq-dev/moq#7653](https://github.com/moq-dev/moq/pull/7653), which saw the
timeout. Milestone: ✅ m0.

Since 2026-11-04 the job fails before any test runs
([run 41979277343](https://github.com/moq-dev/moq/actions/runs/41979277343),
`just rs macos-test`): the test `an_empty_gap_buffer_marks_the_next_samples`
(`rs/moq-audio/src/capture.rs:859`) hands `tokio::spawn` a
`capture::Producer::run()` future that is not `Send` on macOS, because
`Producer` holds the ScreenCaptureKit `SCStream` across an await. It came with
[moq-dev/moq#7706](https://github.com/moq-dev/moq/pull/7706). Decide from the
type whether `Producer` should be `Send` (fix the field) or the test should
drive it without spawning; don't paper over it in the test if real callers
spawn it too. Check why the PR gate didn't catch a macOS-only test build
error, and close that gap if it is cheap.

Then read the job's next run for the websocket timeout. Reproduce it on macOS
before fixing; if it never recurs across a week of scheduled runs, record that
and finish.

Public API: none. Wire: none.

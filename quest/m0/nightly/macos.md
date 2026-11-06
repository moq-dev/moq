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

The capture gap-buffer test now drives `Producer::run()` and its reader in
one task. `Producer::run` documents the macOS local-task requirement, and the
CLI uses a `LocalSet` for capture (`rs/moq-cli/src/main.rs`). ScreenCaptureKit's
handles stay non-Send; there is no new unsafe guarantee.

The Linux-only PR gate cannot detect this macOS-only type error. Platform's
existing `cargo check --all-targets` catches it, but runs nightly and on release
PRs, with manual Platform dispatch available for platform changes. Adding a
native macOS runner to every main PR is not a cheap check. A test-only
non-Send marker in the synthetic Linux stream was also considered: it would
catch test spawns, but not production callers, and would remove coverage of
Linux's supported Send contract from test builds. Keep the real platform
types and the existing non-macOS Send assertions rather than change the test
backend's contract to simulate one macOS constraint. Independent Nightly
error reporting remains owned by
[moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667).

The recorded websocket timeout in
[November 2's scheduled job](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408628726)
was already reproduced and fixed by
[moq-dev/moq#7829](https://github.com/moq-dev/moq/pull/7829), merged November 4.
The client could close before server admission finished; that PR joins both
negotiation results before dropping it, and reports 100/100 native macOS passes.
Do not duplicate that fix or infer a new failure from the old log.

The [November 5 scheduled job](https://github.com/moq-dev/moq/actions/runs/42050683248/job/127326015410)
still stops at the capture test's non-Send compile error, before any tests run.
After the capture fix lands, inspect the next scheduled macOS job. Reproduce
any recurring websocket timeout on macOS before changing it; if none recurs
across a week of scheduled runs, record that evidence and finish.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: after the capture fix lands, verify a green scheduled macOS job
  on main and inspect the websocket test's results across a week of scheduled
  runs. Recommendation: land the reviewed capture fix, then retain this quest
  until the scheduled evidence is available. A branch dispatch can check the
  fix but cannot establish the required scheduled-main history.

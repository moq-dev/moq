# [S] Nightly's gate stops hanging

## Goal

Nightly's `just gate` step finishes instead of hanging until the job timeout,
and when a test does hang, the log names the module that hung.

## Plan

[Run 40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673/job/122198526371)
on 2026-10-27 printed nothing for its last 36 minutes before the 60-minute
timeout. Tooling-test output is buffered, so the hung module is not visible.
Found while landing [moq-dev/moq#6991](https://github.com/moq-dev/moq/pull/6991).

Decided 2026-10-28 (maintainer): one quest for both the fix and the
visibility. Reproduce with `just gate`, fix the hung test at its cause (never
a raised timeout), and make the gate's test output identify each module as it
runs (unbuffered or per-module timestamps) so the next hang names itself.

Public API: none. Wire: none.

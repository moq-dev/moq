# [S] The lite-06 datagram end survives a session close

## Goal

`moq-lite-06: a session close right after a datagram track's end keeps the end`
in `js/net/src/lite/datagram-tracks.test.ts` passes reliably, alone and in the
loaded `js/net` suite. Find why the close intermittently beats the end, fix it
at the cause, then remove the quarantine without weakening the assertion.

## Plan

Planned 2026-11-06 under the maintainer's rule that a known flaky test is
quarantined immediately and its investigation planned. `Check` failed on this
case in
[run 42200239263](https://github.com/moq-dev/moq/actions/runs/42200239263/job/127833434287)
on the unrelated [moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981),
where it was the only failing case in `js/net`. The unchanged module reproduced
it once in eight runs: `cd js/net && bun test src/lite/datagram-tracks.test.ts`.

The case spins 1000 microtasks after `datagrams.finish()`, then aborts the
client, expecting the reader's `recv()` to return `undefined` because the end
already arrived. On failure the reader surfaces the session close instead
(`Session: remote error: 0`). The spin assumes the mock delivers the end in
microtasks, but the subscriber's response loop yields through `@moq/clock`'s
`yieldNow` (a host task) once its budget's 4 ms slice expires, which a loaded
runner can reach before the end is read. Whether that is only the fixture's
wait or a real gap between receiving the end and delivering it is not
established: diagnose before changing the wait or production code, and do not
keep the case passing by loosening the assertion. The lite-05 and lite-07
cases remain enabled, since only lite-06 failed.

Public API: none. Wire: none.

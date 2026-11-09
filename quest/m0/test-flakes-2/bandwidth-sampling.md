# [XS] js/net's bandwidth sampling test holds up under load

## Goal

`js/net/src/connection/bandwidth.test.ts`'s "a connection samples the
transport only while a reservation reads the estimate" passes under load and
runs again.

## Plan

Planned 2026-11-08 (maintainer). It failed under load in three worktrees and
passes on a quiet main. It is quarantined with `test.skip` until this quest
fixes it.

The test counts `getStats` calls over 350 ms of `Manual` clock time and
expects exactly three 100 ms ticks while a reservation reads the estimate, and
none otherwise. Find which step outruns the mocked clock under load: a sample
loop started or stopped on a host task rather than within `advance`, or the
reservation's first read landing after the window opens. Fix it at the cause,
for example by having the test wait on the sampler's start/stop event before
counting, never with a longer window or a retry. Then remove the `test.skip`
and delete this quest.

Reproduction so far (2026-11-08, unskipped locally, nothing committed):

- `bun test src/connection/bandwidth.test.ts --rerun-each 200` on an idle host: 1400 pass.
- The same with 36 busy-loop processes on 24 cores: 1400 pass.
- `--rerun-each 2000` with 72 busy-loop processes: 14000 pass.
- Eight concurrent copies, `--rerun-each 400` each, with 24 busy-loop processes: 22400 pass.
- Full `bun test` in `js/net` under load: 3177 pass, 0 fail.
- None of the 84 failed-job logs downloaded from the last 200 failed runs (`Queue`, `Check`, `Nightly`, `Interop`, and review runs) shows this test failing. The `@moq/net` failures there are other tests.

Ruled out, by reading the code and by instrumenting it:

- The `Manual` clock drains microtasks before each timer fires, and the sampler, the page-wide ticker, and the mock `getStats` all resolve in microtasks. With no host macrotask in between, the count is deterministic: the ticker subscribes at 350 ms and ticks at 450, 550, and 650 ms, inside the window (350, 700].
- No ticker loop is left running from the earlier real-clock tests when this one starts.
- Three WebSocket head-start sleeps from the earlier tests are still pending at the start. A 600 ms real stall before the window fired them and the count stayed 3, so they are not the cause on their own.
- The dynamic imports on the connect path are IETF-only and do not run for this ALPN.

A fix is unverified until the test fails in a run that shows its value.

## Waiting

- Maintainer: keep this test quarantined until a failure log with the received count exists. Find it in the JS test job of a recent run that failed on `@moq/net test: (fail) a connection samples the transport only while a reservation reads the estimate`, with `gh run list --repo moq-dev/moq --status failure` and `gh run view <id> --log-failed`, or in the `bun test` output of one of the three loaded worktrees. The log needs the failing `expect` line and its received value, from whichever assertion of this test failed: usually `expect(await sampled()).toBe(3)`, but an earlier or later one (the `connected` status, a `sampled()` of 0, or a `peek()`) counts too. Recommendation: wait for that log before any fix. For the three-sample line, read the value this way: `4` means one extra `getStats` call landed in the window (a second dial or an extra sample), `2` means a tick was missed (the ticker's phase against the window), and `0` means the sampler never started under the window. Once the log exists, diagnose and fix from it, then remove the `test.skip`. If no log appears, decide whether to delete the test.

Public API: none. Wire: none.

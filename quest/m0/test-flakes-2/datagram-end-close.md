# [S] The datagram-end close test survives load

## Goal

`js/net/src/lite/datagram-tracks.test.ts`'s
`a session close right after a datagram track's end keeps the end` passes
reliably in loaded runs, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules, and its `test.skip` is
removed.

## Plan

Added 2026-11-06 (maintainer's standing rule that flakes join this epic).
[moq-dev/moq#7986](https://github.com/moq-dev/moq/pull/7986)'s Check failed
in this case, whose diff touches only `bench/`, `justfile`, and quest prose.
It reproduces on unchanged `origin/main`, so the flake came from
[moq-dev/moq#7951](https://github.com/moq-dev/moq/pull/7951)'s new case;
the PR quarantines it with every assertion kept.

Reproduction 2026-11-06 on this head and on `git archive origin/main`, from
`js/net`, with six CPU loops on four cores:

- `bun test src/lite/datagram-tracks.test.ts` failed 3 of 4 loaded runs here
  and 3 of 3 on `origin/main`; only the lite-06 draft failed, though the
  case loops over lite-05, lite-06, and lite-07-wip.
- The same filter `-t "a session close right after a datagram track's end
  keeps the end"` passed 10 of 10 idle runs and 8 of 8 loaded runs, so the
  whole file (its 37 other cases) is what makes the window miss.
- The failure rejects with `Session: remote error: 0` at `error.ts`'s
  `closeError` from `connection.ts`'s `abort`, i.e. `reader.recv()`
  rejected instead of ending.

Likely cause, unverified: the case finishes the datagram track and then
drains only microtasks (`for (i < 1000) await Promise.resolve()`) before
`client.abort()`, assuming "the mock delivers the end in microtasks, all
within this host task". The FIN handler awaits `#datagramsRouted`, which
starts with a host-task `yieldNow`; `Subscriber.close` finishes only
datagram entries whose `ended` is already set and aborts the rest. Under
load the FIN is not read before the abort, so `ended` is still unset and
the producer aborts. Check the delivery point before choosing between
waiting on the end's arrival and yielding to the host in the drain.

Reproduce first: temporarily remove the case's `test.skip`, then run the
whole file beside CPU loops, as above. Keep the failing output before making
other changes to the test or the end path. Restore the skip until the fix.

## Additional lite-06 investigation

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
keep the case passing by loosening the assertion. Base subsequently quarantined
all three protocol versions, as described above.

Decided 2026-11-06 (maintainer, /quest-unblock): investigate the additional
[CodeRabbit finding](https://github.com/moq-dev/moq/pull/7981#discussion_r4491539836)
here: an accepted lite-06 `SUBSCRIBE_END` followed by duplicate SETUP can be
overridden by session failure before subscription finalization. Reproduce this
ordering deterministically and preserve the accepted end during shutdown.

Public API: none. Wire: none.

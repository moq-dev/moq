# [S] datagram-tracks' end-before-close case passes under load

## Goal

`a session close right after a datagram track's end keeps the end` in
`js/net/src/lite/datagram-tracks.test.ts` passes under load for every draft in
multi-file runs, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules. Done when its `test.skip`
is removed with its assertions unchanged.

## Plan

Added 2026-11-06 (maintainer's standing rule that flakes join this epic).
[moq-dev/moq#7986](https://github.com/moq-dev/moq/pull/7986) and
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) both hit it in
their checks without touching the code it covers, and each quarantined it with
every assertion kept; this file merges their two quests. It came with
[moq-dev/moq#7951](https://github.com/moq-dev/moq/pull/7951)'s new case.

Reproductions, 2026-11-06, with six CPU loops on four cores:

- On #7986's head and on `origin/main`, from `js/net`: `bun test
  src/lite/datagram-tracks.test.ts` failed 3 of 4 loaded runs there and 3 of
  3 on `main`. The same file filtered to this case passed 10 of 10 idle and 8
  of 8 loaded runs, so the file's other cases are what make the window miss.
- On #7670 at `13a544e41`: 6 of 10 runs, and 4 of 8 on `main` at
  `6ca2ed5e3`, always the lite-06 case, with `subscribe error: id=1
  broadcast=live track=audio error=remote error: 0`. After quarantining
  lite-06 alone, lite-05 failed 1 of 8 further loaded runs the same way, so the
  whole case is quarantined and the fix must cover all three drafts.
- The failure rejects with `Session: remote error: 0` at `error.ts`'s
  `closeError` from `connection.ts`'s `abort`: `reader.recv()` rejected
  instead of ending.

The test ends the publisher's track, then burns microtasks so the end lands
"all within this host task", then closes the session and expects `recv()` to
report the end rather than the close. That holds only when the end's whole
path runs in microtasks, and the failure says it did not: the close reached
the subscriber while `entry.end` was still unset, so `Subscriber.close`
aborted the datagram track. That is the correct answer for a close that
precedes the end, so the assertion is fine and the assumption about when the
end has landed is the suspect. Lead, unverified: a host task still
outstanding, such as the `yieldNow` (`clock.task`) on the publisher's serve
loop or at the start of the FIN handler's `#datagramsRouted`, is starved by
the microtask chain. Lite-06 fits: its SUBSCRIBE carries no range list, so
fewer writes precede the publisher's read of the end. Confirm which host task
is outstanding before choosing the fix; a later host task landing in the
burst, rather than a leftover one, would need a different answer.

Fix it by waiting on an event that proves the subscriber read the end before
closing the session, rather than a microtask count: for example capture the
publisher's `SUBSCRIBE_FINISH` write through the mock as `holdNextAnswer`
does, or await a hook the subscriber calls on the response. Keep the close
between that evidence and `recv()`, so the case still covers the close that
lands after the end. Raising the microtask count, adding a wall-clock wait, or
retrying the test are not fixes.

Reproduce first: temporarily remove the case's `test.skip`, then run the
whole file beside CPU loops, as above. Keep the failing output before
changing the test or the end path, and restore the skip until the fix.

Public API: none. Wire: none.

## Related

- [moq-dev/moq#8000](https://github.com/moq-dev/moq/pull/8000) edited this
  case: it stubs `Budget.prototype.poll` so the publisher's write budget never
  yields a host task, and raises the microtask count to 10000. The quarantined
  case keeps both. Stubbing the budget removes one suspected outstanding
  `yieldNow`, so check whether it is the cause before choosing the fix; the
  raised count alone is not one. On 2026-11-06, ten loaded runs (six busy
  loops pinned to four of 24 cores) passed with and without the stub, so that
  setup no longer reproduces it and cannot tell.

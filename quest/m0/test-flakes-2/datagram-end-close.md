# [S] datagram-tracks' end-before-close case passes under load

## Goal

`a session close right after a datagram track's end keeps the end` in
`js/net/src/lite/datagram-tracks.test.ts` passes under load for every draft in
multi-file runs, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules. Done when the quarantine
from [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) is removed
with its assertions unchanged.

## Plan

Reproduced 2026-11-06 with the whole file beside six busy loops on a four-core
runner. It failed 6 of 10 runs on
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) at `13a544e41`
and 4 of 8 runs on `main` at `6ca2ed5e3`, always the lite-06 case, with
`subscribe error: id=1 broadcast=live track=audio error=remote error: 0`. The
file passes alone. So the draft is the trigger, not the PR, and neither the
test nor the code it covers changed between them: the PR touches no line in
`subscriber.ts` that mentions `entry.end` or `datagrams`.

Lite-06 hits it far more often, but it is not the only draft: after
quarantining lite-06 alone, lite-05 failed 1 of 8 further loaded runs the same
way. So the whole case is quarantined, and the fix must cover all three.

The test ends the publisher's track, then burns 1000 microtasks so the end
lands "all within this host task", then closes the session and expects
`recv()` to report the end rather than the close. That holds only when the
end's whole path runs in microtasks, and the failure says it did not: the
close reached the subscriber while `entry.end` was still unset, so it aborted
the datagram track. That is the correct answer for a close that precedes the
end, so the assertion is fine and the assumption about when the end has landed
is the suspect. A host task (`yieldNow`, which is `clock.task`) still
outstanding on the publisher's serve loop would be starved by the microtask
chain, and lite-06 fits it: its SUBSCRIBE carries no range list, so fewer
writes precede the publisher's read of the end than on lite-05 or lite-07.
Confirm which host task is outstanding before choosing the fix; a later host
task landing in the burst, rather than a leftover one, would need a different
answer.

Fix it by waiting on an event that proves the subscriber read the end before
closing the session, rather than a microtask count: for example capture the
publisher's `SUBSCRIBE_FINISH` write through the mock as `holdNextAnswer`
does, or await a hook the subscriber calls on the response. Keep the close
between that evidence and `recv()`, so the case still covers the close that
lands after the end. Raising the microtask count, adding a wall-clock wait, or
retrying the test are not fixes.

## Related

- [moq-dev/moq#8000](https://github.com/moq-dev/moq/pull/8000), merged
  first, edited this case: it stubs
  `Budget.prototype.poll` so the publisher's write budget never yields a host
  task, and raises the microtask count to 10000. The quarantined case keeps
  both. Stubbing the budget removes the suspected outstanding `yieldNow`, so
  check whether it is the cause before choosing this fix; the raised count
  alone is not one. On 2026-11-06, ten loaded runs (six busy loops pinned to
  four of 24 cores) passed with and without the stub, so that setup no longer
  reproduces it and cannot tell.
- [moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981), a draft,
  quarantines only the lite-06 case and adds its own quest at this same
  path. Whichever lands second keeps one quest and this whole-case
  quarantine, since lite-05 failed too.

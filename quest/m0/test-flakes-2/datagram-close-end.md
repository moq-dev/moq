# [S] js/net's datagram session-close-after-end test holds up under load

## Goal

`js/net/src/lite/datagram-tracks.test.ts`'s "a session close right after a
datagram track's end keeps the end" passes in the merge queue for every
version it covers, and its `test.skip` is removed.

## Plan

Planned 2026-11-09 (maintainer's standing rule that flakes join this epic).
The lite-06 case dropped
[moq-dev/moq#8280](https://github.com/moq-dev/moq/pull/8280) from the merge
queue and was quarantined; [moq-dev/moq#8321](https://github.com/moq-dev/moq/pull/8321)
re-enabled it with no fix after finding no repro; its investigation, which rules
out microtask reordering and names `Budget` before `connect` and `now()` in
`group.ts`/`cache.ts` as the real-time suspects, stays in its PR body. The
lite-05 case then failed the merge queue for
[moq-dev/moq#8276](https://github.com/moq-dev/moq/pull/8276) with `Session:
remote error: 0`: the reader saw the abort before the end. The land fixer's
note on #8276 records 20 clean repeats of the file on a scratch merge, so it
fails only under the queue's load.

The test assumes 10000 microtask turns deliver the publisher's end before
`client.abort()`. Find the step that still crosses a host task under load, and
order the abort after the end by an event the client observes (such as the
reader's track reaching its end state), never by more turns or a sleep. Then
remove the `test.skip` and delete this quest.

Public API: none. Wire: none.

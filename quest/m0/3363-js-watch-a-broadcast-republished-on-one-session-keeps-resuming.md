# [S] js/watch: a broadcast republished under its name on one session keeps resuming

## Goal

A watcher of a broadcast that its publisher drops and republishes under the
same name on the same session re-consumes every generation, not just the
first. The reported shape is an `invisible` `<moq-publish>` toggling `muted`:
with no media left the element unpublishes, and each unmute publishes again.

## Plan

Reported on `@moq/watch` 0.3.2 (#3363); nobody has retested 0.5.3, and three
fixes since target this path: #2617 (`announcedBroadcast`, lite restarts
resolved by publisher identity), #2976 (keep a republished broadcast when its
predecessor closes), #3368 (a refused duplicate namespace does not strand the
first).

Mechanism on main: `js/publish/src/element.ts` derives `hasMedia` from the
audio and video sources, and `Microphone` stops its track when `muted`, so an
`invisible` publisher with `muted` set has no media and `#publishEnabled` goes
false; `js/publish/src/broadcast.ts` `Broadcast.#run` closes the
`Moq.Broadcast.Producer` (the broadcast is unannounced), and unmute creates a
fresh producer and calls `connection.publish(name, broadcast)` again. The
watcher's `js/watch/src/broadcast.ts` `#runBroadcast` delegates to
`conn.announcedBroadcast(name)`, and `js/net/src/announced.ts` keeps a live
handle across a redundant re-announce (`current.closed.peek() === undefined`
continues). If the retraction is coalesced away or lands late, the watcher
holds the dead generation and the wire resets its tracks: play, then close,
until a fresh session (a publisher reload) forces an unambiguous retraction.
dev's `#runBroadcast` builds on `Moq.Origin.Table` and `Announce.Broadcast`
instead, but the publisher chain and the audio source are the same.

- Regression first: a js/net test that publishes, unpublishes, and republishes
  one path on one session three times, and asserts the announced-broadcast
  handle re-consumes each generation and the audio track yields frames after
  each. Run it on main (0.5.3) and dev.
- If it fails, key the re-announce on a generation identity so an announce
  that follows a retraction always swaps the handle. If it passes, keep the
  test and close.
- Manual check per the issue: `just relay`, an `invisible muted`
  `<moq-publish>` toggling `muted`, a `<moq-watch>` on the same name.

## Closes

- [#3363](https://github.com/moq-dev/moq/issues/3363) - close this issue when the quest finishes

## Related

- [#2985](/quest/m1/2985-js-net-path-keyed-publisher-state-goes-stale-when-a.md) - the publisher side of a same-path replacement
- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - sequences across replacements

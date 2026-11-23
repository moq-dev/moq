# [L] A bounded read asks upstream again for a skip its track forgot

## Goal

A bounded read whose local floor or cap keeps out a key under its window owes
that key until a skip accounts for it. It ends once the route has declared the
skip, however long the route took to declare all of it. Today a received
track forgets a skip after the pool's expiry window, and the key is unknown
again. If the route then declares the rest of the window's skips only after
that window, the read stays pending for good. This holds for one cursor and a
front's resumed reader alike, in Rust (`rs/moq-net/src/model/track.rs`) and
JS (`js/net/src/track.ts`).

## Plan

Found by Pullfrog on
[moq-dev/moq#9435](https://github.com/moq-dev/moq/pull/9435), and confirmed
for one plain JS cursor:

1. A track writes groups 0 and 6, and a reader subscribes to time `0..=6` with
   a 10-hour budget.
2. The reader reads 0 and 6, then sets its local range to `6..7`, so keys 1
   to 5 are kept out.
3. The track skips `[3,6)`, then `[1,3)`.

If both skips come together, the read ends. If 60 seconds pass between them,
it stays pending: `[3,6)` expired (`expire_drops` in Rust, `#expireDrops` in
JS) before `[1,3)` arrived. Pullfrog traced the same failure for a front's
resumed reader: its retained ask copy reads the same expired state, and that
PR keeps only the skipped run at the start of each kept-out run.

Recommended: follow the cache rule. Expiry still bounds what the track holds.
A key the read still owes and can no longer prove skipped counts as unknown,
and the read asks upstream for it again, as for any key the cache cannot
answer. Do not pin forgotten runs for a live reader: what a peer declared
would then outlive the pool's bill. Add the case above as a unit test in both
languages, for one cursor and for a resumed reader.

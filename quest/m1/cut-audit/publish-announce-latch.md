# [S] A camera broadcast stays announced across unmute and device swaps

## Goal

Once `<moq-publish>` has announced a camera broadcast, unmuting, unhiding,
picking another device, or a capture retry does not unannounce and
re-announce it, so viewers do not stop and restart on each flap.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited.

`js/publish/src/announce.ts:37-44`: on unmute, `audioEnabled` flips true at
once but the Microphone source stays undefined until `getUserMedia`
resolves, so `ready`, `live`, and `announcing` go false and the broadcast
unannounces until the mic arrives. The same happens on unhide, a device
change, and each retry backoff. `announcing.peek()` bypasses only `settled`,
not `ready`. `announce.test.ts` (about line 211) keeps `source` set, which a
real source never does during a swap, so it passes.

Once announced, latch on the selection (a source chosen and the element
connected), not on momentary track presence; keep the partial-broadcast
guard for the first announce only. Fix the test to clear the source during
the swap.

Public API: none. Wire: none.

## Related

- [Drop announce="never"](/quest/m1/publish-announce-never.md) - the same attribute's other change before the cut

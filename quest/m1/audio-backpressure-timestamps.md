# [XS] Buffered audio backpressure keeps full timestamp precision

## Goal

Buffered audio playback holds an encoded frame until the playhead nears it
for any media timestamp, not only those below 2^31 microseconds (about
35 minutes 47 seconds). Past that point the decode loop today stops waiting
and decodes far ahead, overflowing the floor-sized PCM ring and dropping audio.

## Plan

Found in a 2026-10-22 media audit at
[main 1f5caa7](https://github.com/moq-dev/moq/commit/1f5caa7026bcfd8a2684da3712b384b5f74a7496)
and confirmed by source at
[main d4cdfe4](https://github.com/moq-dev/moq/commit/d4cdfe4368b17cbdf7d2a0c43d24e364a9626480).
`Backpressure.wait` and `Backpressure.advance` in
`js/watch/src/audio/buffer.ts` compare the playhead against
`(timestamp - headroom) | 0`, which truncates an absolute microsecond
timestamp to signed 32 bits. The audit reproduced a wait one second ahead
resolving immediately at 2,150,000,000 and 3,600,000,000 microseconds while
staying pending at small timestamps. Both ring transports share this class.

Drop the `| 0` so the comparison stays in full double precision, and audit the
rest of `js/watch/src/audio` (the rings, `render`, the worklet) for the same
32-bit coercion of an absolute timestamp. A sample index or ring offset may
legitimately be an integer; an absolute media timestamp may not.

Test: a waiter one second ahead stays pending at a timestamp past 2^31 and
2^32 microseconds and resolves once the playhead reaches it, on a stubbed
playhead. No API or wire change.

## Related

- [Audio playout memory](/quest/m1/audio-playout-memory.md) - generalizes the same `Backpressure`; land this first, it is independent

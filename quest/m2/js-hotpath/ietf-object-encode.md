# [S] Encode IETF object timestamps without a WritableStream

## Goal

IETF object header encode allocates nothing beyond the payload. Wire bytes
do not change.

## Plan

`encodeObjectExtensions` in `js/net/src/ietf/object.ts` builds a `Writer`
over a `WritableStream` for every object with a timestamp, pushes ~10-byte
varints as chunks, concatenates, then writes length+bytes. Lite already
writes timestamp varints straight onto the group stream.

Encode properties into `Writer`'s 9-byte scratch (or a 16-byte buffer) the
way `Writer.u62` already does. No stream, no concat.

Acceptance: Bun microbench, `Frame.encode` × 10k at 1 KiB and 100 KiB
payloads. Header encode allocs drop to ~0 beyond the payload. Optional
browser IETF publish vs lite for GC. No wire-byte change.

## Related

- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - container samples
- [Reader buffering](/quest/m2/stream-buffering.md) - `Reader.#fill`
- [Coalesce stream writes](/quest/m2/js-hotpath/write-coalesce.md) - the write syscall, not this header

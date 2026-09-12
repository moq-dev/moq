# [M] Coalesce per-frame WebTransport writes

## Goal

The data path does one `WritableStreamDefaultWriter.write` per object or
frame. Payload is not copied except a small header prefix. Already-buffered
single-chunk writes do not regress.

## Plan

Each media frame is 2–4 `await writer.write()` calls (timestamp, length,
payload, sometimes extensions) in lite `#runGroup` and IETF `Frame.encode`.
Scratch is reused only because writes are awaited, so headers cannot
pipeline.

`Writer.writeFrame(headerFields, payload)` that copies the ~1–20 header
bytes in front of the payload, or a 2-chunk write if the platform supports
it without copy. Keep the primitive `u53` API for control messages.

Acceptance: real WebTransport, not Bun: audio 20 ms + video 30 fps. Count
`write` calls and CPU. One write per object on the data path.

## Related

- [Reader buffering](/quest/m2/stream-buffering.md) - the read side
- [IETF object header encode](/quest/m2/js-hotpath/ietf-object-encode.md) - header construction
- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - harness

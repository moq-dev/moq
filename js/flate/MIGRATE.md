# Migrating @moq/flate

How to update your code for each breaking change, newest first.

## Unreleased

### A compressed group is held to the group budget

`Decoder` holds its whole window, across every frame, to `Group.MAX_GROUP_CACHE_BYTES` (32 MiB) instead of capping each frame at 64 MiB.
`DEFAULT_MAX_FRAME_SIZE`, `DecoderOptions`, and `Decoder.maxFrameSize` are removed; construct with `new Decoder()`.
The producers charge a compressed payload its inflated size: a snapshot value over 32 MiB throws `FrameTooLarge` however small it deflates, and a stream log ends its budget at 32 MiB of inflated payload.

### @moq/binary is @moq/flate

The opaque `Snapshot` and `Stream` tracks moved beside the codec; the wire and the catalog's `binary` section are unchanged.
Import `Snapshot` and `Stream` from `@moq/flate` instead of `@moq/binary`.

### Consumers return `Timed` values (#5099)

The snapshot and stream consumers' `next()` and async iterator yield `{ value, at }`, where `at` is absent on an untimed track.
A snapshot consumer's `next()` yields every state in order; call `latest()` to skip to the newest.
Where you awaited `next()` for the newest bytes, read `(await consumer.latest())?.value`.

### Producers never invent a timestamp (#5070)

`Snapshot.Producer.update` and `Stream.Producer.append` take a `Timed<Uint8Array>`.
Pass `{ value: bytes, at: Time.Timestamp.now() }` instead of the bytes.
Omit `at` only on an untimed track.

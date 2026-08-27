# @moq/binary

Opaque binary payloads over MoQ tracks, in two modes:

- `Snapshot`: **lossy**. One value updated over time; a consumer only gets the most recent one.
- `Stream`: **lossless**. An ordered append-log of self-contained payloads, nothing superseded.

The bytes are opaque: this package frames them onto a track and optionally compresses them, and
never looks inside. For JSON documents reach for [`@moq/json`](../json) instead, which adds RFC 7396
merge-patch deltas on top of the same two modes.

Compression is [`@moq/flate`](../flate), the same group-scoped DEFLATE `@moq/json` uses, so the two
agree on the wire. Interoperable with the Rust `moq-binary` crate.

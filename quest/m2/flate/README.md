# Compressed tracks

## Goal

Any track can be DEFLATE-compressed per group from every language, not only
the JSON modes. A native or C caller publishes and consumes a compressed track
of opaque frames the same way a Rust or browser caller does, and the bytes on
the wire are identical across all of them.

## Plan

`moq-flate` and `@moq/flate` today are a bare codec: `Encoder`/`Decoder` with
`frame(bytes) -> bytes` and a shared window the caller scopes to a group by
hand. Only `moq-json` composes them, so the bindings reach compression through
`compression: bool` on the JSON configs and nothing else. A telemetry, caption,
or sensor track of raw frames has no compressed form outside Rust and JS, and
even there the caller re-derives the group discipline from the crate docs.

The line adds a track wrapper to the crate first, then binds that wrapper. The
codec objects stay as they are; the wrapper owns the per-group window so a
caller cannot desynchronize it. The wire format does not change: a wrapper
group is the raw sync-flushed stream the codec already emits, so a wrapped
producer interoperates with a hand-composed consumer and with `moq-json`.

No wire, catalog, or relay impact. Compression stays invisible to `moq-net`;
a compressed track is announced, routed, and cached like any other.

## Quests

- [Track wrapper](/quest/m2/flate/track.md) - `moq-flate` and `@moq/flate` wrap a track so each group is one compression window without caller bookkeeping
- [Bindings](/quest/m2/flate/bindings.md) - moq-ffi and libmoq publish and subscribe compressed tracks, mirrored through every wrapper

## Related

- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - the libmoq catch-up this line adds one more symbol family to

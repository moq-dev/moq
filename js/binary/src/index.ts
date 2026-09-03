/**
 * Opaque binary payloads over MoQ tracks, in two modes:
 *
 * - {@link Snapshot}: **lossy**. One value updated over time; a consumer only gets the most recent
 *   one. Older values are superseded and dropped.
 * - {@link Stream}: **lossless**. An ordered append-log of self-contained payloads, delivered in
 *   order with nothing superseded. Bounded by the group cache: see {@link Stream} for what that
 *   costs a consumer that falls behind.
 *
 * Pick {@link Snapshot} when consumers care about "what is the value now" (a poster image, a
 * serialized state blob) and {@link Stream} when they care about every payload (an event log, a
 * sequence of samples).
 *
 * The bytes are opaque: this package frames them onto a track and optionally compresses them, and
 * never looks inside. For JSON documents reach for `@moq/json` instead, which adds RFC 7396
 * merge-patch deltas on top of the same two modes.
 *
 * Compression is `@moq/flate`, the same group-scoped DEFLATE `@moq/json` uses, so the two agree on
 * the wire: each group is one raw DEFLATE stream, sync-flushed at every frame boundary. A
 * {@link Stream} therefore compresses each payload against the earlier ones in its group, while a
 * {@link Snapshot} group holds a single self-contained value. Interoperable with the Rust
 * `moq-binary` crate.
 *
 * @module
 */

export * as Snapshot from "./snapshot/index.ts";
export * as Stream from "./stream/index.ts";

/**
 * Opaque byte tracks over MoQ, optionally compressed with group-scoped DEFLATE, in two modes:
 *
 * - {@link Snapshot}: **lossy**. One value updated over time; a consumer only gets the most recent
 *   one. Older values are superseded and dropped.
 * - {@link Stream}: **lossless**. An ordered append-log of self-contained payloads, delivered in
 *   order with nothing superseded. Bounded by the group cache: see {@link Stream} for what that
 *   costs a consumer that falls behind.
 *
 * Pick {@link Snapshot} when consumers care about "what is the value now" (a poster image, a
 * serialized state blob) and {@link Stream} when they care about every payload (an event log, a
 * sequence of samples). The bytes are opaque: the tracks frame them and optionally compress them,
 * and never look inside. For JSON documents reach for `@moq/json` instead, which adds RFC 7396
 * merge-patch deltas on top of the same two modes and codec.
 *
 * Compression is opt-in per track ({@link Compression}), so the package name is not a promise that
 * every track is deflated: `"none"` writes the bytes through untouched.
 *
 * ## Codec
 *
 * Underneath, {@link Encoder}/{@link Decoder} compress a sequence of frame payloads into a single
 * raw DEFLATE ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html)) stream, sync-flushed at each
 * frame boundary, using {@link https://github.com/nodeca/pako | pako}. Every frame is self-delimited
 * (byte-aligned, the window retained) while later frames reuse the earlier ones as context, so a
 * stream of similar payloads compresses far better than each payload alone. Create a fresh pair per
 * independent stream (in moq-net terms, per group). A {@link Stream} therefore compresses each
 * payload against the earlier ones in its group, while a {@link Snapshot} group holds a single
 * self-contained value.
 *
 * This is plain raw DEFLATE with a `Z_SYNC_FLUSH` after each frame, so any peer using the same
 * primitive (the Rust `moq-flate` crate, zlib's sync flush) interoperates on the wire. There is no
 * length prefix: the caller frames each slice (moq-net already does).
 *
 * A sync flush always ends in the fixed 4-byte marker `00 00 ff ff`. {@link Encoder.frame} drops it
 * and {@link Decoder.frame} re-appends it, saving 4 bytes per frame, the same trick
 * [RFC 7692](https://www.rfc-editor.org/rfc/rfc7692.html#section-7.2.1) (permessage-deflate) uses. A
 * small slice can still inflate enormously, so {@link Decoder.frame} caps the inflated output as it
 * is produced. pako is synchronous, so the whole codec is synchronous.
 *
 * @module
 */

export {
	DEFAULT_LEVEL,
	DEFAULT_MAX_FRAME_SIZE,
	Decoder,
	type DecoderOptions,
	Encoder,
	type EncoderOptions,
} from "./codec.ts";
export type { Compression } from "./compression.ts";
export * as Snapshot from "./snapshot/index.ts";
export * as Stream from "./stream/index.ts";

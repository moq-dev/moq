/**
 * Lossy latest-value JSON publishing over MoQ tracks.
 *
 * One JSON value updated over time, for consumers that only care about the current state (a
 * catalog, a status document). This mode is **lossy** by design: a consumer yields only the most
 * recent value. A late joiner (or a consumer that falls behind) jumps straight to the newest
 * group and collapses any buffered backlog into a single yield, and older groups are dropped
 * entirely. Intermediate updates are never replayed. For an ordered log where every record is
 * preserved, use the `Stream` module instead.
 *
 * On the wire the value is a series of self-contained groups: frame 0 is a full snapshot and any
 * following frames are RFC 7396 JSON Merge Patch deltas applied in order. Interoperable with the
 * Rust `moq_json::snapshot`.
 *
 * {@link Producer} and {@link Consumer} own a track: hand one a track and it manages the groups for
 * you. {@link Encoder} and {@link Decoder} are the same logic without the track. The encoder turns
 * values into {@link Encoded} frame payloads and says where the group boundaries fall; the decoder
 * reconstructs a value from those payloads. Reach for them when something else is already in charge
 * of the track.
 *
 * Encoding advances state that the frame's consumers depend on, so {@link Encoder.update} hands back
 * a {@link Pending} the caller commits once the write succeeds. Leaving one uncommitted
 * resynchronizes the encoder, which keeps a frame that never reached the wire from desyncing the
 * stream.
 *
 * @module
 */

export { Consumer } from "./consumer.ts";
export { Decoder } from "./decoder.ts";
export { type Config, type Encoded, Encoder, type Pending } from "./encoder.ts";
export { Producer, type ProducerConfig } from "./producer.ts";

/**
 * Lossless append-log JSON publishing over MoQ tracks: the counterpart to the lossy `Snapshot`
 * module. Instead of one value updated over time, a stream is an ordered log of self-contained
 * records that preserves everything; every {@link Producer.append} writes one JSON object as one
 * frame, nothing is ever superseded, and a {@link Consumer} yields every record in order.
 *
 * The whole log rides a **single group** that is never rolled: with {@link ProducerConfig.compression}
 * on, that one group is one DEFLATE window, so every record compresses against the earlier ones.
 * There is deliberately no group rolling (and so no catch-up machinery); a caller that wants to
 * bound the record rate throttles at the source. Interoperable on the wire with the Rust
 * `moq_json::stream`.
 *
 * {@link Producer} and {@link Consumer} own a track. {@link Encoder} and {@link Decoder} are the
 * same logic without it, for when something else is already in charge of the track; they carry the
 * shared DEFLATE window and nothing else, since a log has no group boundaries to report.
 *
 * @module
 */

export { Consumer } from "./consumer.ts";
export { type ConsumerConfig, Decoder } from "./decoder.ts";
export { Encoder, type ProducerConfig } from "./encoder.ts";
export { Producer } from "./producer.ts";

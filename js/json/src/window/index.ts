/**
 * Sliding-window JSON publishing over MoQ tracks.
 *
 * A window is an ordered run of records the publisher appends to the back of and drops from the
 * front of. Unlike the `Stream` module, which preserves a log forever in one group, and `Snapshot`,
 * which keeps only the latest value, a window keeps a bounded stretch of records and lets a reader
 * join it at any point. Interoperable on the wire with the Rust `moq_json::window`.
 *
 * The obvious alternative is an append-only log that rolls its group and re-seeds the new one with
 * the records it still holds. That breaks the reader: re-seeded records are indistinguishable from
 * new ones, so a reader that was keeping up receives them twice. This mode exists to make the
 * restatement explicit, so a reader can tell "you already have these" from "here is another one".
 *
 * Every frame is a tagged op. Each group opens with a `reset` naming the retained records and the
 * absolute `offset` of the first, followed by `push` and `pop` ops. Only the reset carries an index:
 * a push takes the next one and a pop drops from the front, both positional against the reset, which
 * is sound because the group-scoped DEFLATE window already makes a mid-group join undecodable.
 * Trimming is therefore an op, not a group boundary, so dropping a record costs one small frame
 * inside the shared compression window instead of a roll that would throw that window away.
 *
 * The publisher rolls a group when the ops in it outgrow {@link ProducerConfig.opRatio} times the
 * reset that opened it. That is purely a compression decision: there is no caller-driven cut and no
 * age bound, and a {@link Consumer} never surfaces it.
 *
 * Every index is reported exactly once: a `push` event when a record arrives, `pop` when it leaves,
 * and `skip` when it existed but was dropped before this reader saw it. A reader that keeps up sees
 * pushes and pops; one that joins late, or falls a group behind, learns from the reset's offset
 * which records it will never get rather than silently missing them.
 *
 * {@link Producer} and {@link Consumer} own a track. {@link Encoder} and {@link Decoder} are the
 * same logic without it, for when something else is already in charge of the track.
 *
 * @module
 */

export { Consumer } from "./consumer.ts";
export { type ConsumerConfig, Decoder, type Event } from "./decoder.ts";
export { type Encoded, Encoder, type Op, type Pending, type ProducerConfig } from "./encoder.ts";
export { Producer } from "./producer.ts";

/**
 * Lossless append-log binary publishing over MoQ tracks.
 *
 * An ordered log of opaque payloads, for consumers that care about every one (an event log, a
 * sequence of samples). Nothing is ever superseded: a consumer yields each payload in the order it
 * was appended. For a latest-value document, use the `Snapshot` module instead.
 *
 * On the wire the log rides a **single group** that is never rolled, one payload per frame. A
 * payload that cannot be written ends the track rather than opening a second group: a log missing a
 * record is not lossless, and a gap dressed up as a complete log is worse than a visible failure.
 * With {@link ProducerConfig.compression} on, that one group is one DEFLATE window, so each payload
 * compresses against the earlier ones and a run of similar payloads shrinks sharply.
 *
 * @module
 */

export { Consumer, type ConsumerConfig } from "./consumer.ts";
export { Producer, type ProducerConfig } from "./producer.ts";

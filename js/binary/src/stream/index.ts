/**
 * Lossless append-log binary publishing over MoQ tracks.
 *
 * An ordered log of opaque payloads, for consumers that care about every one (an event log, a
 * sequence of samples). Nothing is ever superseded: a consumer yields each payload in the order it
 * was appended. For a latest-value document, use the `Snapshot` module instead.
 *
 * On the wire the log normally rides a **single group**, one payload per frame. A later group
 * continues the log rather than superseding an earlier one, which is what separates this from
 * `Snapshot`; the producer only rolls a group to recover from a frame it could not write. With
 * {@link ProducerConfig.compression} on, each group is one DEFLATE window, so each payload
 * compresses against the earlier ones and a run of similar payloads shrinks sharply.
 *
 * @module
 */

export { Consumer, type ConsumerConfig } from "./consumer.ts";
export { Producer, type ProducerConfig } from "./producer.ts";

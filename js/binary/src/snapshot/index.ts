/**
 * Lossy latest-value binary publishing over MoQ tracks.
 *
 * One opaque value updated over time, for consumers that only care about the current state (a
 * poster image, a serialized state blob). This mode is **lossy** by design: a consumer yields only
 * the most recent value. A late joiner (or a consumer that falls behind) jumps straight to the
 * newest group, and older groups are dropped entirely. For an ordered log where every payload is
 * preserved, use the `Stream` module instead.
 *
 * On the wire each value is one group holding one frame, so a group is self-contained and a
 * consumer never needs an older one. With {@link ProducerConfig.compression} on, that frame is its
 * own raw DEFLATE stream; there is no window to share across a single-frame group.
 *
 * @module
 */

export { Consumer, type ConsumerConfig } from "./consumer.ts";
export { Producer, type ProducerConfig } from "./producer.ts";

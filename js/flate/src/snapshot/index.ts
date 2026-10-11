/**
 * Lossy latest-value opaque publishing over MoQ tracks.
 *
 * One opaque value updated over time, for consumers that care about the state rather than every
 * payload (a poster image, a serialized state blob). This mode is **lossy** by design: a new group
 * supersedes the older ones, which are dropped. {@link Consumer.next} yields every value it still
 * receives, in group order, each with its frame's timestamp, and {@link Consumer.latest} jumps
 * straight to the newest group. For an ordered log where every payload is preserved, use the
 * `Stream` module instead.
 *
 * On the wire each value is one group holding one frame, so a group is self-contained and a
 * consumer never needs an older one. With {@link Config.compression} `"deflate"`, that frame is its
 * own raw DEFLATE stream; there is no window to share across a single-frame group.
 *
 * @module
 */

export { Consumer } from "./consumer.ts";
export { type Config, Producer } from "./producer.ts";

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
 * @module
 */

export { Consumer } from "./consumer.ts";
export { type Config, Producer } from "./producer.ts";

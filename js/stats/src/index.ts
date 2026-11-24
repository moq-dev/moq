/**
 * Read the traffic stats a MoQ relay publishes, the JavaScript counterpart of the Rust `moq-stats`
 * consumer.
 *
 * {@link Nodes} discovers each relay node's stats broadcast under a prefix (`.stats` by default);
 * {@link Consumer} wraps one node's broadcast and yields typed frames per track. Every counter is
 * cumulative, so a rate is `delta / delta_t` and a live count is `started - ended`. See
 * https://doc.moq.dev/concept/stats for the format.
 *
 * @module
 */

export { BucketReader, type BucketUpdate, Consumer, Reader } from "./consumer.ts";
export * from "./frame.ts";
export { type NodeEvent, Nodes } from "./node.ts";
export {
	bucketTrack,
	PREFIX,
	parseBucketTrack,
	parseNodePath,
	prefixTrack,
	presenceTrack,
	totalsTrack,
} from "./track.ts";

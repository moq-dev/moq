import { Time, type Track } from "@moq/net";

/**
 * `at`, or one unit of its timescale past the newest start on `track` when it doesn't pass that:
 * a write stamped from a clock can land twice in one millisecond, and two groups never share a
 * start. `at` is in the track's timescale. Mirrors the Rust `moq_mux::start::next`.
 */
export function after(track: Track.Producer, at: Time.Timestamp): Time.Timestamp {
	const newest = track.largest()?.group;
	if (newest === undefined || at.scale === undefined || BigInt(Time.floorAt(at, at.scale)) > newest) return at;
	return new Time.Timestamp(Number(newest + 1n), at.scale);
}

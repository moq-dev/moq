// The timestamp check both halves share, so a frame either accepts is one `@moq/net` accepts.
import { Error as NetError, Time } from "@moq/net";

/**
 * `timestamp` at the track's `timescale`, rounded as the wire rounds it. Mirrors Rust's
 * `moq_net::group::on_track`, checked before a frame is signed or encrypted so a refused
 * timestamp spends nothing.
 *
 * Throws `TimestampMismatch` when the frame's timedness disagrees with the track's, or a
 * `RangeError` when the converted value leaves the safe range.
 */
export function onTrack(
	timestamp: Time.Timestamp | undefined,
	timescale: Time.Timescale | undefined,
): Time.Timestamp | undefined {
	if ((timescale !== undefined) !== (timestamp !== undefined)) throw new NetError.TimestampMismatch();
	if (timestamp === undefined || timescale === undefined) return undefined;
	return new Time.Timestamp(Math.round(timestamp.as(timescale)), timescale);
}

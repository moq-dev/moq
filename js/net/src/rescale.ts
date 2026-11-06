// Shared by stream and datagram tracks, without a cycle through their model modules.
import { TimestampMismatch } from "./error.ts";
import { floorAt, type Timescale, Timestamp } from "./time.ts";

/**
 * `timestamp` at `info`'s timescale, rounded down, as Rust's `Timestamp::convert` does.
 *
 * A track is all timed or all untimed, so a frame must match it: a timestamp on an untimed track,
 * none on a timed one, or one the scale can't hold throws {@link TimestampMismatch}. A writer that
 * changes state before its frame reaches the track, such as signing or encrypting it, rescales
 * first so a refused timestamp changes nothing. Mirrors Rust's track and datagram `Info::rescale`.
 */
export function rescale(
	info: { timescale?: Timescale | null },
	timestamp: Timestamp | undefined,
): Timestamp | undefined {
	const timescale = info.timescale ?? undefined;
	if ((timescale !== undefined) !== (timestamp !== undefined)) throw new TimestampMismatch();
	if (timestamp === undefined || timescale === undefined) return undefined;
	try {
		return new Timestamp(floorAt(timestamp, timescale), timescale);
	} catch (cause) {
		throw new TimestampMismatch({ cause });
	}
}

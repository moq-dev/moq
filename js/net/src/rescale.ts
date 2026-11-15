// Shared by stream and datagram tracks, without a cycle through their model modules.
import { TimestampMismatch } from "./error.ts";
import { floorAt, keyOf, type Timescale, Timestamp } from "./time.ts";

/**
 * `timestamp` at `info`'s timescale, rounded down, as Rust's `Timestamp::convert` does.
 *
 * A track is all timed or all scale-less, so a frame must match it: an unscaled timestamp on a
 * timed track, a scaled one on a scale-less track, or one the scale can't hold throws
 * {@link TimestampMismatch}. A scale-less track's timestamp passes through unchanged. A writer
 * that changes state before its frame reaches the track, such as signing or encrypting it,
 * rescales first so a refused timestamp changes nothing. Mirrors Rust's track and datagram
 * `Info::rescale`.
 */
export function rescale(info: { timescale?: Timescale | null }, timestamp: Timestamp): Timestamp {
	const timescale = info.timescale ?? undefined;
	if ((timescale !== undefined) !== (timestamp.scale !== undefined)) throw new TimestampMismatch();
	if (timescale === undefined) return timestamp;
	try {
		return new Timestamp(floorAt(timestamp, timescale), timescale);
	} catch (cause) {
		throw new TimestampMismatch({ cause });
	}
}

/**
 * {@link rescale} for a frame of group `key` (or a datagram of sequence `key`): a scale-less
 * track's frame carries its group's key, so any other throws {@link TimestampMismatch}. A writer
 * that changes state before its frame reaches the group checks this first, as with
 * {@link rescale}. Mirrors Rust's `Info::rescale_in`.
 */
export function rescaleIn(info: { timescale?: Timescale | null }, timestamp: Timestamp, key: bigint): Timestamp {
	const rescaled = rescale(info, timestamp);
	const unscaled = keyOf(rescaled);
	if (unscaled !== undefined && unscaled !== key) throw new TimestampMismatch();
	return rescaled;
}

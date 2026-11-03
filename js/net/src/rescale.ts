// Kept apart from track.ts so group.ts can import it without a cycle; track.ts re-exports it.
import { TimestampMismatch } from "./error.ts";
import { Timestamp } from "./time.ts";
import type { Info } from "./track.ts";

/**
 * `timestamp` at `info`'s {@link Info.timescale}, rounded down, as Rust's `Timestamp::convert` does.
 *
 * A track is all timed or all untimed, so a frame must match it: a timestamp on an untimed track,
 * none on a timed one, or one the scale can't hold throws {@link TimestampMismatch}. A writer that
 * changes state before its frame reaches the track, such as signing or encrypting it, rescales
 * first so a refused timestamp changes nothing. Mirrors the Rust `track::Info::rescale`.
 */
export function rescale(info: Pick<Info, "timescale">, timestamp: Timestamp | undefined): Timestamp | undefined {
	const timescale = info.timescale ?? undefined;
	if ((timescale !== undefined) !== (timestamp !== undefined)) throw new TimestampMismatch();
	if (timestamp === undefined || timescale === undefined) return undefined;
	try {
		return new Timestamp(Math.floor(timestamp.as(timescale)), timescale);
	} catch (cause) {
		throw new TimestampMismatch({ cause });
	}
}

// Kept apart from track.ts so group.ts can import it without a cycle; track.ts re-exports it.
import { TimestampMismatch } from "./error.ts";
import { type Timescale, Timestamp } from "./time.ts";
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
		return new Timestamp(floorAt(timestamp, timescale), timescale);
	} catch (cause) {
		throw new TimestampMismatch({ cause });
	}
}

/**
 * `timestamp`'s value at `scale`, rounded down exactly as Rust's integer `Timestamp::convert`.
 *
 * The whole part converts in integer math, so a value already aligned to `scale` keeps it
 * instead of losing a unit to floating-point division (4611686021000000 ns is 4611686021 ms,
 * where `as` gives 4611686020.999999). Throws a `RangeError` past the safe integer range.
 */
export function floorAt(timestamp: Timestamp, scale: Timescale): number {
	const { value, scale: from } = timestamp;
	const whole = Math.floor(value);
	if (from === scale) return whole;
	const product = BigInt(whole) * BigInt(scale);
	const quotient = product / BigInt(from);
	// The remainder and the fraction stay below one unit at `scale` each, well inside float precision.
	const rest = Number(product % BigInt(from)) + (value - whole) * scale;
	const result = Number(quotient) + Math.floor(rest / from);
	if (!Number.isSafeInteger(result))
		throw new RangeError(`timestamp ${value} at ${from} exceeds the safe range at ${scale}`);
	return result;
}

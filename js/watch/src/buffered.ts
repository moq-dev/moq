/**
 * Buffered time-range helpers shared by the watch backends.
 *
 * Kept in a leaf module (depends only on `@moq/net`) so the backend and the
 * per-track MSE sources can share them without importing each other.
 *
 * @module
 */
import * as Moq from "@moq/net";

/** A single buffered time range. */
export interface BufferedRange {
	start: Moq.Time.Milli;
	end: Moq.Time.Milli;
}

/** Serializable representation of DOM `TimeRanges`. */
export type BufferedRanges = BufferedRange[];

/** Convert a DOM `TimeRanges` into a {@link BufferedRanges}. */
export function timeRangesToArray(ranges: TimeRanges): BufferedRanges {
	const result: BufferedRange[] = [];

	for (let i = 0; i < ranges.length; i++) {
		const start = Moq.Time.Milli.fromSecond(ranges.start(i) as Moq.Time.Second);
		const end = Moq.Time.Milli.fromSecond(ranges.end(i) as Moq.Time.Second);

		result.push({ start, end });
	}
	return result;
}

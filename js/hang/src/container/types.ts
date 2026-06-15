import { Time } from "@moq/net";

/** A decoded media frame: codec payload plus its presentation timestamp and keyframe flag. */
export interface Frame {
	/** The codec bitstream payload. */
	data: Uint8Array;
	/** Presentation timestamp in microseconds. */
	timestamp: Time.Micro;
	/** Whether this frame is a keyframe (can be decoded standalone). */
	keyframe: boolean;
}

/** A contiguous span of buffered media, in milliseconds. */
export interface BufferedRange {
	/** Start of the range in milliseconds. */
	start: Time.Milli;
	/** End of the range in milliseconds. */
	end: Time.Milli;
}

/** An ordered list of buffered time ranges. */
export type BufferedRanges = BufferedRange[];

/** Merge two sets of buffered ranges into a single sorted, non-overlapping list. */
export function mergeBufferedRanges(a: BufferedRanges, b: BufferedRanges): BufferedRanges {
	if (a.length === 0) return b;
	if (b.length === 0) return a;

	const result: BufferedRanges = [];
	const all = [...a, ...b].sort((x, y) => x.start - y.start);

	for (const range of all) {
		const last = result.at(-1);
		if (last && last.end >= range.start) {
			last.end = Time.Milli.max(last.end, range.end);
		} else {
			result.push({ ...range });
		}
	}

	return result;
}

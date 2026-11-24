import type { Duration, Instant } from "@moq/clock";

/** A decoded media frame: codec payload plus its presentation timestamp and keyframe flag. */
export interface Frame {
	/** The codec bitstream payload. */
	payload: Uint8Array;
	/** Presentation timestamp: how far into the media timeline this frame starts. */
	timestamp: Duration;
	/** Whether this frame opens a group, or is a video keyframe. Audio carries it only on a group start. */
	keyframe: boolean;

	/**
	 * How long this frame occupies the presentation timeline. CMAF carries a
	 * per-sample duration, and Legacy audio reads one from a codec that states it;
	 * otherwise Legacy and LOC report later endpoints through Consumer.next().end.
	 * The consumer adds it to `timestamp` to learn how far a group has presented,
	 * so it can advance to a newer group as soon as the gap is covered instead of
	 * waiting out the max delay budget.
	 */
	duration?: Duration;
}

/** A media frame carrying its local transport ingress time. */
export interface ReceivedFrame extends Frame {
	/** When the frame came off the transport, before reordering or decoder backpressure. */
	arrival: Instant;
}

/** A contiguous span of buffered media, on the presentation timeline. */
export interface BufferedRange {
	/** Start of the range. */
	start: Duration;
	/** End of the range. */
	end: Duration;
}

/** An ordered list of buffered time ranges. */
export type BufferedRanges = BufferedRange[];

/** Merge two sets of buffered ranges into a single sorted, non-overlapping list. */
export function mergeBufferedRanges(a: BufferedRanges, b: BufferedRanges): BufferedRanges {
	if (a.length === 0) return b;
	if (b.length === 0) return a;

	const result: BufferedRanges = [];
	const all = [...a, ...b].sort((x, y) => x.start.cmp(y.start));

	for (const range of all) {
		const last = result.at(-1);
		if (last?.end.ge(range.start)) {
			last.end = last.end.max(range.end);
		} else {
			result.push({ ...range });
		}
	}

	return result;
}

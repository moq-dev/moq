import { Time } from "@moq/net";

export interface Frame {
	data: Uint8Array;
	timestamp: Time.Micro;
	keyframe: boolean;

	// How long this frame occupies the presentation timeline. CMAF carries a
	// per-sample duration; containers that don't (Legacy) leave it undefined,
	// which the consumer treats as zero. The consumer adds it to `timestamp` to
	// learn how far a group has presented, so it can advance to a newer group as
	// soon as the gap is covered instead of waiting out the latency budget.
	duration?: Time.Micro;
}

export interface BufferedRange {
	start: Time.Milli;
	end: Time.Milli;
}

export type BufferedRanges = BufferedRange[];

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

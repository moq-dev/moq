import { Time } from "@moq/net";

export interface Frame {
	data: Uint8Array;
	timestamp: Time.Micro;
	keyframe: boolean;
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

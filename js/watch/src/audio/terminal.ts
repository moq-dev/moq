import type { Time } from "@moq/net";

interface SampleSpan {
	readonly timestamp: number;
	readonly sampleRate: number;
	readonly numberOfFrames: number;
}

/** Return the source frames before an exclusive terminal endpoint. */
export function terminalFrames(sample: SampleSpan, end?: Time.Micro): number {
	if (end === undefined) return sample.numberOfFrames;
	if (end <= sample.timestamp) return 0;

	const duration = end - sample.timestamp;
	const frames = Math.round((duration * sample.sampleRate) / 1_000_000);
	return Math.min(frames, sample.numberOfFrames);
}

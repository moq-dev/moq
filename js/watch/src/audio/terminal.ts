import type { Time } from "@moq/net";

interface SampleSpan {
	readonly timestamp: number;
	readonly sampleRate: number;
	readonly numberOfFrames: number;
}

interface Update {
	readonly discontinuity: number;
	readonly end?: Time.Micro;
}

/** Track discontinuities and their ordered terminal endpoint metadata. */
export class Terminal {
	#discontinuity = 0;
	#end?: Time.Micro;

	/** The exclusive source endpoint most recently received. */
	get end(): Time.Micro | undefined {
		return this.#end;
	}

	/** Reset state for a new container consumer. */
	clear(): void {
		this.#discontinuity = 0;
		this.#end = undefined;
	}

	/** Apply one ordered consumer result and report whether its timeline rewound. */
	update(next: Update): boolean {
		const reset = next.discontinuity !== this.#discontinuity;
		if (reset) {
			this.#discontinuity = next.discontinuity;
			this.#end = undefined;
		}
		if (next.end !== undefined) this.#end = next.end;
		return reset;
	}
}

/** Return the source frames before an exclusive terminal endpoint. */
export function terminalFrames(sample: SampleSpan, end?: Time.Micro): number {
	if (end === undefined) return sample.numberOfFrames;
	if (end <= sample.timestamp) return 0;

	const duration = end - sample.timestamp;
	const frames = Math.round((duration * sample.sampleRate) / 1_000_000);
	return Math.min(frames, sample.numberOfFrames);
}

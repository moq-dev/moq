import type { Time } from "@moq/net";

interface SampleSpan {
	readonly timestamp: number;
	readonly sampleRate: number;
	readonly numberOfFrames: number;
}

interface Update {
	readonly discontinuity: number;
	readonly end?: Time.Micro;
	readonly frame?: { readonly timestamp: Time.Micro };
}

/** The portion of decoded audio that belongs to the source timeline. */
export interface DecodedSpan {
	/** Source timestamp of the first retained frame. */
	readonly timestamp: Time.Micro;
	/** Decoded frames to skip before copying. */
	readonly frameOffset: number;
	/** Decoded frames to retain after the offset. */
	readonly frames: number;
}

/** Track codec epochs and map decoded output onto the source timeline. */
export class Terminal {
	#discontinuity = 0;
	#end?: Time.Micro;
	#epoch?: Time.Micro;
	#preSkip = 0;
	#preSkipRemaining?: number;

	/** The exclusive source endpoint most recently received. */
	get end(): Time.Micro | undefined {
		return this.#end;
	}

	/** Reset state for a new container consumer with Opus pre-skip in 48 kHz frames. */
	clear(preSkip = 0): void {
		this.#discontinuity = 0;
		this.#end = undefined;
		this.#preSkip = preSkip;
		this.#resetEpoch();
	}

	/** Apply one ordered consumer result and report whether its codec epoch changed. */
	update(next: Update): boolean {
		const reset = next.discontinuity !== this.#discontinuity;
		if (reset) {
			this.#discontinuity = next.discontinuity;
			this.#end = undefined;
			this.#resetEpoch();
		}
		if (next.frame && this.#epoch === undefined) this.#epoch = next.frame.timestamp;
		if (next.end !== undefined) this.#end = next.end;
		return reset;
	}

	/** Remove codec pre-skip and terminal padding from one decoded sample. */
	span(sample: SampleSpan): DecodedSpan {
		const epoch = this.#epoch ?? (sample.timestamp as Time.Micro);
		this.#epoch = epoch;

		const delayFrames = Math.floor((this.#preSkip * sample.sampleRate) / 48_000);
		const remaining = this.#preSkipRemaining ?? delayFrames;
		const frameOffset = Math.min(remaining, sample.numberOfFrames);
		this.#preSkipRemaining = remaining - frameOffset;

		const delay = Math.round((delayFrames * 1_000_000) / sample.sampleRate);
		const timestamp = Math.max(epoch, sample.timestamp - delay) as Time.Micro;
		let frames = sample.numberOfFrames - frameOffset;
		if (this.#end !== undefined) {
			if (this.#end <= timestamp) {
				frames = 0;
			} else {
				const duration = this.#end - timestamp;
				const terminalFrames = Math.round((duration * sample.sampleRate) / 1_000_000);
				frames = Math.min(frames, terminalFrames);
			}
		}

		return { timestamp, frameOffset, frames };
	}

	#resetEpoch(): void {
		this.#epoch = undefined;
		this.#preSkipRemaining = undefined;
	}
}

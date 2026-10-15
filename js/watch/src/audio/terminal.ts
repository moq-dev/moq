import type { Time } from "@moq/net";

interface SampleSpan {
	readonly timestamp: number;
	readonly sampleRate: number;
	readonly numberOfFrames: number;
}

/** The parts of a container read the terminal tracks. */
type Update =
	| { readonly kind: "frame"; readonly group: number; readonly frame: { readonly timestamp: Time.Micro } }
	| { readonly kind: "gap" }
	| { readonly kind: "end"; readonly group: number; readonly end: Time.Micro };

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
	// Whether a frame arrived since the last reset, so there is an epoch to end.
	#started = false;
	#end?: Time.Micro;
	// The group that carried `#end`. It bounds only the terminal packets that follow it there.
	#endGroup?: number;
	#epoch?: Time.Micro;
	#preSkip = 0;
	#preSkipRemaining?: number;

	/** The exclusive source endpoint most recently received. */
	get end(): Time.Micro | undefined {
		return this.#end;
	}

	/** Reset state for a new container consumer with Opus pre-skip in 48 kHz frames. */
	clear(preSkip = 0): void {
		this.#started = false;
		this.#end = undefined;
		this.#endGroup = undefined;
		this.#preSkip = preSkip;
		this.#resetEpoch();
	}

	/** Apply one ordered consumer read and report whether a codec epoch ended before it. */
	update(read: Update): boolean {
		if (read.kind === "gap") return this.#restart();
		if (read.kind === "end") {
			this.#end = read.end;
			this.#endGroup = read.group;
			return false;
		}
		// A later group after a declared end comes from a fresh encoder; the end's own group carries
		// the stopped encoder's final packets.
		const reset = this.#end !== undefined && read.group !== this.#endGroup && this.#restart();
		this.#started = true;
		this.#epoch ??= read.frame.timestamp;
		return reset;
	}

	// Start a new epoch, reporting whether one was running.
	#restart(): boolean {
		const started = this.#started;
		this.#started = false;
		this.#end = undefined;
		this.#endGroup = undefined;
		this.#resetEpoch();
		return started;
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

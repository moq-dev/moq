import type { Micro } from "@moq/clock";

interface SampleSpan {
	readonly timestamp: number;
	readonly sampleRate: number;
	readonly numberOfFrames: number;
}

/** The parts of a container read the terminal tracks. */
type Update =
	| { readonly kind: "frame"; readonly group: number; readonly frame: { readonly timestamp: Micro } }
	| { readonly kind: "gap" }
	| { readonly kind: "end"; readonly group: number; readonly end: Micro };

/** The portion of decoded audio that belongs to the source timeline. */
export interface DecodedSpan {
	/** Source timestamp of the first retained frame. */
	readonly timestamp: Micro;
	/** Decoded frames to skip before copying. */
	readonly frameOffset: number;
	/** Decoded frames to retain after the offset. */
	readonly frames: number;
}

/** Track codec epochs and map decoded output onto the source timeline. */
export class Terminal {
	// Whether media has started on this timeline.
	#started = false;
	#end?: Micro;
	// The group that carried `#end`. It bounds only the terminal packets that follow it there.
	#endGroup?: number;
	#epoch?: Micro;
	#preSkip = 0;
	#preSkipRemaining?: number;
	#warmup = 0 as Micro;
	#prime = true;
	#before?: Micro;

	/** The exclusive source endpoint most recently received. */
	get end(): Micro | undefined {
		return this.#end;
	}

	/** Reset with Opus pre-skip in 48 kHz frames and warmup in microseconds. */
	clear(preSkip = 0, warmup = 0 as Micro): void {
		this.#started = false;
		this.#end = undefined;
		this.#endGroup = undefined;
		this.#preSkip = preSkip;
		this.#warmup = warmup;
		this.#prime = true;
		this.#before = undefined;
		this.#resetEpoch();
	}

	/** Apply one ordered consumer read and report whether a codec epoch ended before it. */
	update(read: Update): boolean {
		if (read.kind === "gap") {
			this.#prime = true;
			this.#before = undefined;
			this.#resetEpoch();
			return this.#started;
		}
		if (read.kind === "end") {
			this.#end = read.end;
			this.#endGroup = read.group;
			return false;
		}
		// A later group after a declared end comes from a fresh encoder; the end's own group carries
		// the stopped encoder's final packets.
		const reset = this.#endGroup !== undefined && read.group > this.#endGroup;
		if (reset) this.#restart();
		this.#started = true;
		this.#epoch ??= read.frame.timestamp;
		if (this.#prime) {
			this.#before = (read.frame.timestamp + this.#warmup) as Micro;
			this.#prime = false;
		}
		return reset;
	}

	// Start a fresh timeline after its declared end.
	#restart(): void {
		this.#started = false;
		this.#end = undefined;
		this.#endGroup = undefined;
		this.#before = undefined;
		this.#resetEpoch();
	}

	/** Remove codec pre-skip and terminal padding from one decoded sample. */
	span(sample: SampleSpan): DecodedSpan {
		const epoch = this.#epoch ?? (sample.timestamp as Micro);
		this.#epoch = epoch;

		const delayFrames = Math.floor((this.#preSkip * sample.sampleRate) / 48_000);
		const remaining = this.#preSkipRemaining ?? delayFrames;
		let frameOffset = Math.min(remaining, sample.numberOfFrames);
		this.#preSkipRemaining = remaining - frameOffset;

		const delay = Math.round((delayFrames * 1_000_000) / sample.sampleRate);
		let timestamp = Math.max(epoch, sample.timestamp - delay) as Micro;
		let frames = sample.numberOfFrames - frameOffset;
		if (this.#before !== undefined && timestamp < this.#before) {
			const trim = Math.min(frames, Math.ceil(((this.#before - timestamp) * sample.sampleRate) / 1_000_000));
			frameOffset += trim;
			frames -= trim;
			timestamp = (timestamp + Math.round((trim * 1_000_000) / sample.sampleRate)) as Micro;
		}
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

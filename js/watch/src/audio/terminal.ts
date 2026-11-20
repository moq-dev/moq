import type { Duration } from "@moq/clock";

interface SampleSpan {
	readonly timestamp: number;
	readonly sampleRate: number;
	readonly numberOfFrames: number;
}

/** The parts of a container read the terminal tracks. */
type Update =
	| { readonly kind: "frame"; readonly group: bigint; readonly frame: { readonly timestamp: Duration } }
	| { readonly kind: "gap" }
	| { readonly kind: "end"; readonly group: bigint; readonly end: Duration };

// How far, in microseconds, pre-roll output may end past the pre-roll: it absorbs the restamped
// timestamps' rounding, and is shorter than any codec frame (Opus's shortest is 2.5 ms).
const PREROLL_SLACK = 1_000;

/** An encoded frame handed to the decoder, on the timeline the decoder saw it. */
export interface Encoded {
	/** Microseconds. */
	readonly timestamp: number;
	/** Microseconds. */
	readonly duration: number;
	readonly payload: Uint8Array;
}

/** The portion of decoded audio that belongs to the source timeline. */
export interface DecodedSpan {
	/** Source timestamp of the first retained frame, in microseconds. */
	readonly timestamp: number;
	/** Decoded frames to skip before copying. */
	readonly frameOffset: number;
	/** Decoded frames to retain after the offset. */
	readonly frames: number;
}

/** Track codec epochs and map decoded output onto the source timeline. */
export class Terminal {
	// Whether media has started on this timeline.
	#started = false;
	#end?: number;
	// The group that carried `#end`. It bounds only the terminal packets that follow it there.
	#endGroup?: bigint;
	#epoch?: number;
	#preSkip = 0;
	#preSkipRemaining?: number;
	#warmup = 0;
	#prime = true;
	#before?: number;
	// The newest frames decoded, back to back and covering the warmup, which re-prime the decoder a
	// gap restarts. Their output is pre-roll for the warmup to trim instead of the audio after the gap.
	#history: Encoded[] = [];
	#covered = 0;
	#preroll: Encoded[] = [];
	// Where the pre-roll ends: output before it only primes the decoder.
	#prerollEnd?: number;

	/** How far decoded output lands before its frame timestamp: the Opus pre-skip, or zero. */
	get delay(): number {
		return Math.round((this.#preSkip * 1_000_000) / 48_000);
	}

	/** The exclusive source endpoint most recently received. */
	get end(): number | undefined {
		return this.#end;
	}

	/** Reset with Opus pre-skip in 48 kHz frames and warmup in microseconds. */
	clear(preSkip = 0, warmup = 0): void {
		this.#started = false;
		this.#end = undefined;
		this.#endGroup = undefined;
		this.#preSkip = preSkip;
		this.#warmup = warmup;
		this.#prime = true;
		this.#before = undefined;
		this.#forget();
		this.#resetEpoch();
	}

	/** Whether applying a read would end a codec epoch, as `update` reports. */
	ends(read: Update): boolean {
		if (read.kind === "gap") return this.#started;
		return read.kind === "frame" && this.#endGroup !== undefined && read.group > this.#endGroup;
	}

	/** Apply one ordered consumer read and report whether a codec epoch ended before it. */
	update(read: Update): boolean {
		if (read.kind === "gap") {
			this.#prime = true;
			this.#before = undefined;
			this.#preroll = [];
			this.#prerollEnd = undefined;
			this.#resetEpoch();
			return this.#started;
		}
		if (read.kind === "end") {
			this.#end = read.end.asMicros();
			this.#endGroup = read.group;
			return false;
		}
		// A later group after a declared end comes from a fresh encoder; the end's own group carries
		// the stopped encoder's final packets.
		const reset = this.ends(read);
		if (reset) this.#restart();
		this.#started = true;
		const timestamp = read.frame.timestamp.asMicros();
		this.#epoch ??= timestamp;
		if (this.#prime) {
			const primed = this.#reprime(timestamp);
			// On the timeline `span` maps output onto, where this frame's output lands: a pre-skip early,
			// since an Opus packet decodes to the encoder's lookahead before its timestamp.
			this.#before = timestamp - this.delay + Math.max(0, this.#warmup - primed);
			this.#prime = false;
		}
		return reset;
	}

	/**
	 * Note a frame handed to the decoder, with its duration in microseconds if known. A frame of unknown
	 * duration cannot be restamped, so the pre-roll starts after it.
	 */
	decoded(frame: { readonly timestamp: Duration; readonly payload: Uint8Array }, duration?: Duration): void {
		if (this.#warmup === 0) return;
		const micros = duration?.asMicros() ?? 0;
		if (micros <= 0) {
			this.#history = [];
			this.#covered = 0;
			return;
		}
		this.#history.push({ timestamp: frame.timestamp.asMicros(), duration: micros, payload: frame.payload });
		this.#covered += micros;
		while (this.#covered - this.#history[0].duration >= this.#warmup) {
			this.#covered -= this.#history[0].duration;
			this.#history.shift();
		}
	}

	/** Take the frames to decode, output discarded, before the first frame after a gap. */
	preroll(): Encoded[] {
		const preroll = this.#preroll;
		this.#preroll = [];
		return preroll;
	}

	/**
	 * Whether a decoded sample is pre-roll output alone, which only primes the decoder. A sample
	 * reaching past the pre-roll holds the audio after the gap, so it is not.
	 */
	primes(sample: SampleSpan): boolean {
		const end = this.#prerollEnd;
		if (end === undefined) return false;
		const sampleEnd = sample.timestamp + (sample.numberOfFrames * 1_000_000) / sample.sampleRate;
		if (sampleEnd <= end + PREROLL_SLACK) return true;
		this.#prerollEnd = undefined;
		return false;
	}

	// Restamp the history to end where the first frame after a gap starts, as the pre-roll that
	// re-primes the restarted decoder, returning how much of the warmup it covers. Chrome stamps an
	// AudioDecoder's output from its first input, so the pre-roll carries the timestamps it plays at.
	#reprime(timestamp: number): number {
		let start = timestamp;
		const preroll: Encoded[] = [];
		for (let i = this.#history.length - 1; i >= 0; i--) {
			start -= this.#history[i].duration;
			preroll[i] = { ...this.#history[i], timestamp: start };
		}
		this.#history = preroll;
		this.#preroll = preroll;
		if (preroll.length === 0) return 0;
		this.#epoch = start;
		this.#prerollEnd = timestamp;
		return timestamp - start;
	}

	#forget(): void {
		this.#history = [];
		this.#covered = 0;
		this.#preroll = [];
		this.#prerollEnd = undefined;
	}

	// Start a fresh codec epoch after its declared end, on the same timeline.
	#restart(): void {
		this.#started = false;
		this.#end = undefined;
		this.#endGroup = undefined;
		this.#before = undefined;
		// A fresh encoder's first packets need no frames of the stopped one ahead of them.
		this.#forget();
		this.#resetEpoch();
	}

	/** Remove codec pre-skip and terminal padding from one decoded sample. */
	span(sample: SampleSpan): DecodedSpan {
		const epoch = this.#epoch ?? sample.timestamp;
		this.#epoch = epoch;

		const delayFrames = Math.floor((this.#preSkip * sample.sampleRate) / 48_000);
		const remaining = this.#preSkipRemaining ?? delayFrames;
		let frameOffset = Math.min(remaining, sample.numberOfFrames);
		this.#preSkipRemaining = remaining - frameOffset;

		const delay = Math.round((delayFrames * 1_000_000) / sample.sampleRate);
		let timestamp = Math.max(epoch, sample.timestamp - delay);
		let frames = sample.numberOfFrames - frameOffset;
		if (this.#before !== undefined && timestamp < this.#before) {
			const trim = Math.min(frames, Math.ceil(((this.#before - timestamp) * sample.sampleRate) / 1_000_000));
			frameOffset += trim;
			frames -= trim;
			timestamp = timestamp + Math.round((trim * 1_000_000) / sample.sampleRate);
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

import { Micro } from "@moq/clock";
import type { AudioFrame } from "./capture";

type Entry = { frame: AudioFrame; end: Micro };

/** @internal Codec-sized PCM retained until demanded, bounded by time and sample count. */
export class Backlog {
	readonly #retention: Micro;
	readonly #limit: number;
	#frames: (Entry | undefined)[] = [];
	#head = 0;
	#samples = 0;

	/** Retain at most this much audio at the codec's sample rate. */
	constructor(retention: Micro, sampleRate: number) {
		this.#retention = retention;
		this.#limit = Math.ceil((retention * sampleRate) / 1_000_000);
	}

	/** Store one complete frame, expiring against both capture time and the clock. */
	push(frame: AudioFrame, end: Micro, now: Micro): void {
		this.#frames.push({ frame, end });
		this.#samples += frame.channels[0].length;
		this.expire(Micro(Math.max(now, end)));
		while (this.#samples > this.#limit) this.shift();
	}

	/** Drop expired frames even when capture has stopped delivering samples. */
	expire(now: Micro): void {
		while (this.deadline !== undefined && this.deadline <= now) this.shift();
	}

	/** The oldest frame's expiry, or undefined while empty. */
	get deadline(): Micro | undefined {
		const first = this.#frames[this.#head];
		return first && Micro(first.end + this.#retention);
	}

	/** Remove the oldest retained frame. */
	shift(): AudioFrame | undefined {
		const entry = this.#frames[this.#head];
		if (!entry) return;
		this.#frames[this.#head++] = undefined;
		this.#samples -= entry.frame.channels[0].length;
		// Amortized constant-time eviction without keeping expired PCM alive.
		if (this.#head * 2 >= this.#frames.length) {
			this.#frames = this.#frames.slice(this.#head);
			this.#head = 0;
		}
		return entry.frame;
	}
}

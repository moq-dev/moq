import { Micro } from "@moq/clock";
import type { AudioFrame } from "./capture";

type Entry = { frame: AudioFrame; end: Micro; cut: boolean };

/** @internal Codec-sized PCM retained until demanded, bounded by time and sample count. */
export class Backlog {
	readonly #retention: Micro;
	readonly #limit: number;
	#frames: (Entry | undefined)[] = [];
	#head = 0;
	#samples = 0;
	#evicted = false;

	/** Retain at most this much audio at the codec's sample rate. */
	constructor(retention: Micro, sampleRate: number) {
		this.#retention = retention;
		this.#limit = Math.ceil((retention * sampleRate) / 1_000_000);
	}

	/**
	 * Store one complete frame, expiring against both capture time and the clock. `cut` marks the
	 * first frame after a break in the timeline, such as a disable.
	 */
	push(frame: AudioFrame, end: Micro, now: Micro, cut = false): void {
		this.#frames.push({ frame, end, cut });
		this.#samples += frame.channels[0].length;
		this.expire(Micro(Math.max(now, end)));
		while (this.#samples > this.#limit) this.#evict();
	}

	/** Drop expired frames even when capture has stopped delivering samples. */
	expire(now: Micro): void {
		while (this.deadline !== undefined && this.deadline <= now) this.#evict();
	}

	/** Whether frames were dropped unencoded since the last call, leaving a gap in the timeline. */
	takeEvicted(): boolean {
		const evicted = this.#evicted;
		this.#evicted = false;
		return evicted;
	}

	#evict(): void {
		this.#evicted = true;
		this.shift();
	}

	/** Whether nothing is retained. */
	get empty(): boolean {
		return this.#frames[this.#head] === undefined;
	}

	/**
	 * Whether the oldest frame starts after a break, clearing the mark. An evicted mark needs no
	 * carrying forward: the eviction is a break of its own ({@link takeEvicted}).
	 */
	takeCut(): boolean {
		const first = this.#frames[this.#head];
		if (!first?.cut) return false;
		first.cut = false;
		return true;
	}

	/** The oldest frame's expiry, or undefined while empty. */
	get deadline(): Micro | undefined {
		const first = this.#frames[this.#head];
		return first && Micro(first.end + this.#retention);
	}

	/** The oldest retained frame, left in place. */
	peek(): AudioFrame | undefined {
		return this.#frames[this.#head]?.frame;
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

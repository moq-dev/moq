import type { Duration } from "@moq/clock";
import type { AudioFrame } from "./capture";

type Entry = { frame: AudioFrame; end: Duration; cut: boolean };

/** @internal Codec-sized PCM retained until demanded, bounded by time and sample count. */
export class Backlog {
	readonly #retention: Duration;
	readonly #limit: number;
	#frames: (Entry | undefined)[] = [];
	#head = 0;
	#samples = 0;
	#evicted = false;

	/** Retain at most this much audio at the codec's sample rate. */
	constructor(retention: Duration, sampleRate: number) {
		this.#retention = retention;
		this.#limit = Math.ceil(retention.asSecs() * sampleRate);
	}

	/**
	 * Store one complete frame, expiring against both capture time and the clock. `cut` marks the
	 * first frame after a break in the timeline, such as a disable.
	 */
	push(frame: AudioFrame, end: Duration, now: Duration, cut = false): void {
		this.#frames.push({ frame, end, cut });
		this.#samples += frame.channels[0].length;
		this.expire(now.max(end));
		while (this.#samples > this.#limit) this.#evict();
	}

	/**
	 * Drop expired frames even when capture has stopped delivering samples. A `budget` shorter than
	 * the retention also drops what a subscriber that far behind the live edge would find stale.
	 */
	expire(now: Duration, budget: Duration = this.#retention): void {
		const age = budget.min(this.#retention);
		for (;;) {
			const first = this.#frames[this.#head];
			if (!first || first.end.add(age).gt(now)) return;
			this.#evict();
		}
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
	get deadline(): Duration | undefined {
		const first = this.#frames[this.#head];
		return first?.end.add(this.#retention);
	}

	/** The oldest retained frame and where it ends, left in place. */
	peek(): { readonly frame: AudioFrame; readonly end: Duration } | undefined {
		return this.#frames[this.#head];
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

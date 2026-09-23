import type { Broadcast } from "./broadcast";

const WINDOW = 10_000_000; // 10 seconds in microseconds.

type Sample = { at: number; lateness: number };

// The minimum encode lateness across every rendition in one broadcast. The queue is ordered by
// lateness so its head is the minimum in the last window, and each sample enters/leaves once.
export class JitterClock {
	#samples: Sample[] = [];
	#head = 0;

	observe(timestamp: number, now: number): number {
		const cutoff = now - WINDOW;
		while (this.#head < this.#samples.length && this.#samples[this.#head].at < cutoff) this.#head++;

		const lateness = now - timestamp;
		while (this.#samples.length > this.#head) {
			const last = this.#samples.at(-1);
			if (!last || last.lateness < lateness) break;
			this.#samples.pop();
		}
		if (this.#head === this.#samples.length) {
			this.#samples = [];
			this.#head = 0;
		} else if (this.#head > 1024 && this.#head * 2 > this.#samples.length) {
			this.#samples = this.#samples.slice(this.#head);
			this.#head = 0;
		}
		this.#samples.push({ at: now, lateness });
		return Math.max(0, lateness - this.#samples[this.#head].lateness);
	}
}

const clocks = new WeakMap<Broadcast, JitterClock>();

export function observeFlush(broadcast: Broadcast, timestamp: number): number {
	let clock = clocks.get(broadcast);
	if (!clock) {
		clock = new JitterClock();
		clocks.set(broadcast, clock);
	}
	return clock.observe(timestamp, performance.now() * 1000);
}

// One rendition's advertised maximum. The clock is shared; the maximum is deliberately not.
export class RenditionJitter {
	#maximum = 0;

	get current(): number | undefined {
		return this.#maximum || undefined;
	}

	observe(broadcast: Broadcast, timestamp: number): number | undefined {
		const rounded = Math.ceil(observeFlush(broadcast, timestamp) / 1000);
		if (rounded <= this.#maximum) return undefined;
		this.#maximum = rounded;
		return rounded;
	}
}

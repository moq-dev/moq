const WINDOW = 10_000_000; // 10 seconds in microseconds.

type Sample = { at: number; lateness: number };

// One rendition's recent minimum encode lateness. The queue is ordered by lateness so its head is
// the minimum in the last window, and each sample enters/leaves once.
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

// One rendition's advertised maximum spread above its own recent minimum lateness.
export class RenditionJitter {
	#clock = new JitterClock();
	#maximum = 0;

	get current(): number | undefined {
		return this.#maximum || undefined;
	}

	observe(timestamp: number): number | undefined {
		const rounded = Math.ceil(this.#clock.observe(timestamp, performance.now() * 1000) / 1000);
		if (rounded <= this.#maximum) return undefined;
		this.#maximum = rounded;
		return rounded;
	}
}

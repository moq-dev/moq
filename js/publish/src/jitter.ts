import * as Catalog from "@moq/hang/catalog";

const WINDOW = 10_000_000; // 10 seconds in microseconds.

type Sample = { at: number; lateness: number };

// The minimum flush lateness over the last window, so a media clock drifting against the wall clock
// does not ratchet forever. The queue is ordered by lateness so its head is the minimum, and each
// sample enters/leaves once.
//
// Every js/publish timestamp is `performance.now()`, so lateness compares across renditions and one
// window can be shared by a whole broadcast.
export class Baseline {
	#samples: Sample[] = [];
	#head = 0;

	// Insert a lateness observed at `now` and return the window's minimum.
	observe(lateness: number, now: number): number {
		const cutoff = now - WINDOW;
		while (this.#head < this.#samples.length && this.#samples[this.#head].at < cutoff) this.#head++;

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
		return this.#samples[this.#head].lateness;
	}
}

// One rendition's catalog `jitter` and `delay`, each a lifetime maximum in whole milliseconds.
// Mirrors `moq_mux::catalog::Estimator`.
export class Estimator {
	#baseline = new Baseline();
	#jitter = 0;
	#delay = 0;

	// The catalog fields measured so far, each absent until nonzero.
	get estimate(): { jitter?: Catalog.U53; delay?: Catalog.U53 } {
		return {
			jitter: this.#jitter ? Catalog.u53(this.#jitter) : undefined,
			delay: this.#delay ? Catalog.u53(this.#delay) : undefined,
		};
	}

	// Measure a frame handed to the transport now, against the `broadcast` baseline every rendition
	// in the catalog shares. Jitter is the spread above this rendition's own recent minimum lateness,
	// so a constant encoder delay is not jitter; delay is how far that minimum trails the broadcast's.
	// Returns whether either estimate rose.
	flush(timestamp: number, broadcast: Baseline, now = performance.now() * 1000): boolean {
		const lateness = now - timestamp;
		const earliest = broadcast.observe(lateness, now);
		const minimum = this.#baseline.observe(lateness, now);

		const jitter = Math.ceil((lateness - minimum) / 1000);
		const delay = Math.ceil((minimum - earliest) / 1000);
		if (jitter <= this.#jitter && delay <= this.#delay) return false;

		this.#jitter = Math.max(this.#jitter, jitter);
		this.#delay = Math.max(this.#delay, delay);
		return true;
	}
}

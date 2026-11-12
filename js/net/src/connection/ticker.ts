import { type Duration, now, sleepUntil } from "@moq/clock";

/**
 * One page-wide ticker shared by every caller with the same period.
 *
 * Each subscriber's callback runs once per tick, all in the same task, so N
 * connections sampling `getStats()` cost one timer wakeup per period instead of N
 * loops ticking out of phase. The loop runs only while a subscriber is registered.
 */
class Ticker {
	readonly #period: Duration;
	readonly #subscribers = new Set<() => void>();
	#abort?: AbortController;

	constructor(period: Duration) {
		this.#period = period;
	}

	/** Runs `fn` on every tick until the returned dispose is called. */
	subscribe(fn: () => void): () => void {
		this.#subscribers.add(fn);
		if (!this.#abort) this.#start();
		return () => {
			if (!this.#subscribers.delete(fn)) return;
			if (this.#subscribers.size === 0) {
				this.#abort?.abort();
				this.#abort = undefined;
			}
		};
	}

	#start() {
		const abort = new AbortController();
		this.#abort = abort;
		void (async () => {
			let next = now().saturatingAdd(this.#period);
			for (;;) {
				await sleepUntil(next, { signal: abort.signal }).catch(() => {});
				if (abort.signal.aborted) return;
				// Snapshot so a callback that unsubscribes doesn't skip a neighbour.
				for (const fn of [...this.#subscribers]) {
					try {
						fn();
					} catch (err) {
						console.error("ticker callback error", err);
					}
				}
				next = now().saturatingAdd(this.#period);
			}
		})();
	}
}

// Keyed by nanoseconds, since equal durations are distinct objects.
const tickers = new Map<number, Ticker>();

/** Subscribes `fn` to the page-wide ticker for `period`; returns its dispose. */
export function tick(period: Duration, fn: () => void): () => void {
	if (period.isZero()) throw new RangeError("ticker period must be positive");
	let ticker = tickers.get(period.asNanos());
	if (!ticker) {
		ticker = new Ticker(period);
		tickers.set(period.asNanos(), ticker);
	}
	return ticker.subscribe(fn);
}

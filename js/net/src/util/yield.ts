import { yieldNow } from "@moq/clock";

// The slice measures real CPU time spent without yielding, which a manual clock never advances:
// on one, the budget would never yield and a backlog could starve the host's own tasks.
// biome-ignore lint/style/noRestrictedGlobals: host CPU time, not logical time.
const host = performance;

/** A shared work budget that lets browser tasks run during a memory-resident backlog.
 * @internal
 */
export class Budget {
	#checks = 0;
	#start = host.now();
	#pending: Promise<void> | undefined;

	/** Yield after a work slice, checking the clock in batches to keep the hot path cheap. */
	poll(): Promise<void> | undefined {
		if (this.#pending !== undefined) return this.#pending;
		if (++this.#checks % 32 !== 0 || host.now() - this.#start < 4) return;
		this.#pending = yieldNow().then(() => {
			this.#start = host.now();
			this.#pending = undefined;
		});
		return this.#pending;
	}
}

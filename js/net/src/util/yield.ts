/**
 * Resolve after the host's next task, so I/O the host already queued runs first.
 *
 * Scheduling, not time: a manual clock must not hold it back, so it uses the host's timer directly.
 *
 * @internal
 */
export function nextTask(): Promise<void> {
	// biome-ignore lint/style/noRestrictedGlobals: a zero-delay task boundary, not a clock read.
	return new Promise<void>((resolve) => setTimeout(resolve, 0));
}

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
		this.#pending = new Promise<void>((resolve) => {
			// Message ports schedule a task without the nested timer's minimum delay.
			const channel = new MessageChannel();
			channel.port1.onmessage = () => {
				channel.port1.close();
				channel.port2.close();
				this.#start = host.now();
				this.#pending = undefined;
				resolve();
			};
			channel.port2.postMessage(null);
		});
		return this.#pending;
	}
}

/** A shared work budget that lets browser tasks run during a memory-resident backlog.
 * @internal
 */
export class Budget {
	#checks = 0;
	#start = performance.now();
	#pending: Promise<void> | undefined;

	/** Yield after a work slice, checking the clock in batches to keep the hot path cheap. */
	poll(): Promise<void> | undefined {
		if (this.#pending !== undefined) return this.#pending;
		if (++this.#checks % 32 !== 0 || performance.now() - this.#start < 4) return;
		this.#pending = new Promise<void>((resolve) => {
			// Message ports schedule a task without the nested timer's minimum delay.
			const channel = new MessageChannel();
			channel.port1.onmessage = () => {
				channel.port1.close();
				channel.port2.close();
				this.#start = performance.now();
				this.#pending = undefined;
				resolve();
			};
			channel.port2.postMessage(null);
		});
		return this.#pending;
	}
}

/**
 * Runs tasks one at a time in submit order: one AEAD call in flight, completions in order.
 *
 * A producer's depth one keeps a frame's ordinal equal to its `@moq/net` write order without
 * reservations; the queue is the callers' own pending promises, so awaiting each write is the
 * backpressure. A consumer runs only opens through it, never net waits.
 */
export class Pump {
	#tail: Promise<unknown> = Promise.resolve();

	run<T>(task: () => Promise<T> | T): Promise<T> {
		const result = this.#tail.then(task);
		this.#tail = result.catch(() => {});
		return result;
	}
}

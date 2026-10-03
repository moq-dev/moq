import { Once } from "@moq/signals";

/** Tracks session-local announcement loops until their withdrawals are acknowledged. @internal */
export class Withdrawal {
	readonly closing = new Once<true>();
	#tasks = new Set<Promise<void>>();

	track(task: Promise<void>): Promise<void> {
		const tracked = task.finally(() => this.#tasks.delete(tracked));
		this.#tasks.add(tracked);
		return tracked;
	}

	async close(): Promise<void> {
		if (!this.closing.peek()) this.closing.set(true);
		// Settle every loop, not just the first to fail: one rejected withdrawal must not
		// strand its siblings mid-write, which is the same lost-DONE this class prevents.
		// The first failure still rejects, so the caller learns the drain was incomplete.
		const settled = await Promise.allSettled(this.#tasks);
		const failed = settled.find((result) => result.status === "rejected");
		if (failed?.status === "rejected") throw failed.reason;
	}
}

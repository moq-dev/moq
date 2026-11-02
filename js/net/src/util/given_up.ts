import { type Dispose, type GetPromise, race } from "@moq/signals";
import type { Consumer as GroupConsumer } from "../group.ts";

// Why to stop writing `group`: the subscription's budget gave it up, or it failed for good
// before it was read whole (evicted, or its copy aborted with no route to continue it).
function reason(group: GroupConsumer): Error | undefined {
	const closed = group.closed.peek();
	return group.expired.peek() ?? (closed instanceof Error ? closed : undefined);
}

/**
 * Why a publisher should stop writing one group: `expired`, or `closed` with an error. A clean
 * close is no reason. Settles with that reason, which latches. Mirrors the Rust publishers'
 * `poll_given_up`.
 *
 * Watches the group once while it is served, until {@link close}, rather than once per write:
 * a fresh watch per frame would subscribe to the track's drift inputs and judge the budget
 * again each time. A served group spends its life parked on a read or a write, and a parked
 * read watches the same inputs, so this judges no more often than a watch per write.
 *
 * @internal
 */
export class GivenUp implements GetPromise<Error> {
	#group: GroupConsumer;
	#stop?: Error;
	#listeners = new Set<(value: Error | undefined) => void>();
	#unwatch: Dispose[];

	constructor(group: GroupConsumer) {
		this.#group = group;
		this.#unwatch = [group.expired.subscribe(() => this.#check()), group.closed.subscribe(() => this.#check())];
	}

	/**
	 * Run a write of the group, settling with why to stop it instead if that comes first. A
	 * group already given up never starts the write. Rejects if the write does.
	 *
	 * `cancel`, if given, also ends the wait, settling with `undefined` and leaving the write
	 * abandoned: the caller learns why from its own state. Racing it here saves a caller with
	 * its own reasons to stop a second race per write. Make it once per group and reuse it: a
	 * long-lived promise only gains and drops a listener per race.
	 */
	async write(write: () => Promise<unknown>, cancel?: Promise<false>): Promise<Error | undefined> {
		// Judged afresh rather than read from the watch: a wall-clock deadline can pass before
		// its timer wakes the watch.
		const stop = this.#stop ?? reason(this.#group);
		if (stop) return stop;
		const done = write().then(() => true as const);
		const result = await race(cancel === undefined ? [done, this] : [done, this, cancel]);
		return result === true || result === false ? undefined : result;
	}

	/** Stop watching the group, once its publisher no longer writes it. */
	close() {
		for (const dispose of this.#unwatch) dispose();
		this.#unwatch = [];
	}

	peek(): Error | undefined {
		return this.#stop;
	}

	// It settles once, so every listener hears it at most once.
	subscribe(fn: (value: Error | undefined) => void): Dispose {
		if (this.#stop) return () => {};
		this.#listeners.add(fn);
		return () => this.#listeners.delete(fn);
	}

	changed(): Promise<Error | undefined>;
	changed(fn: (value: Error | undefined) => void): Dispose;
	changed(fn?: (value: Error | undefined) => void): Promise<Error | undefined> | Dispose {
		if (!fn) return new Promise((resolve) => this.subscribe(resolve));
		return this.subscribe(fn);
	}

	// biome-ignore lint/suspicious/noThenProperty: GivenUp is intentionally awaitable.
	then<R1 = Error, R2 = never>(
		onFulfilled?: ((value: Error) => R1 | PromiseLike<R1>) | null,
		onRejected?: ((reason: unknown) => R2 | PromiseLike<R2>) | null,
	): PromiseLike<R1 | R2> {
		const stop = this.#stop;
		const settled = stop
			? Promise.resolve(stop)
			: new Promise<Error>((resolve) => this.subscribe((value) => value && resolve(value)));
		return settled.then(onFulfilled, onRejected);
	}

	#check(): void {
		if (this.#stop) return;
		const stop = reason(this.#group);
		if (!stop) return;
		this.#stop = stop;
		this.close();
		const listeners = [...this.#listeners];
		this.#listeners.clear();
		for (const fn of listeners) fn(stop);
	}
}

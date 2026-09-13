import { DEFAULT_PUMP_DEPTH, DEFAULT_PUMP_QUEUE } from "./constants.ts";

type Job<T> = {
	id: number;
	start: () => Promise<T>;
	resolve: (value: T) => void;
	reject: (reason: unknown) => void;
	settled: boolean;
};

/**
 * Bounded FIFO of async AEAD operations.
 *
 * Up to {@link Pump.depth} operations run at once. Completions are released in submit
 * order so media is never reordered. Further submits wait; once {@link Pump.queue}
 * waiters are already parked, submit throws rather than growing without bound.
 */
export class Pump {
	/** Maximum concurrent operations. */
	readonly depth: number;
	/** Maximum parked submits waiting for a slot. */
	readonly queue: number;

	#nextId = 0;
	#nextRelease = 0;
	#running = 0;
	#waiting: Job<unknown>[] = [];
	#active: Job<unknown>[] = [];
	#blocked = new Map<number, { resolve: () => void }>();
	#failed?: Error;
	#slotWaiters: Array<() => void> = [];

	/** Create a pump with the documented defaults, or explicit bounds. */
	constructor(options: { depth?: number; queue?: number } = {}) {
		this.depth = options.depth ?? DEFAULT_PUMP_DEPTH;
		this.queue = options.queue ?? DEFAULT_PUMP_QUEUE;
		if (!Number.isInteger(this.depth) || this.depth < 1) {
			throw new RangeError(`pump depth must be a positive integer: ${this.depth}`);
		}
		if (!Number.isInteger(this.queue) || this.queue < 0) {
			throw new RangeError(`pump queue must be a non-negative integer: ${this.queue}`);
		}
	}

	/** In-flight operations plus parked waiters. */
	get size(): number {
		return this.#running + this.#waiting.length;
	}

	/** True when a further {@link submit} would be refused as saturated. */
	get saturated(): boolean {
		return this.#waiting.length >= this.queue && this.#running >= this.depth;
	}

	/**
	 * Run `op` with at most {@link depth} in flight, resolving in submit order.
	 * Throws if the pump has failed or the waiter queue is full.
	 */
	submit<T>(op: () => Promise<T>): Promise<T> {
		if (this.#failed) return Promise.reject(this.#failed);
		if (this.#waiting.length >= this.queue && this.#running >= this.depth) {
			return Promise.reject(new Error("e2ee pump saturated"));
		}

		const id = this.#nextId++;
		return new Promise<T>((resolve, reject) => {
			this.#waiting.push({
				id,
				start: op,
				resolve: resolve as (value: unknown) => void,
				reject,
				settled: false,
			});
			this.#kick();
		});
	}

	/** Reject waiters and in-flight work. Later submits fail with the same error. */
	close(reason: Error): void {
		if (this.#failed) return;
		this.#failed = reason;
		const waiting = this.#waiting.splice(0);
		for (const job of waiting) this.#reject(job, reason);
		for (const job of this.#active.splice(0)) this.#reject(job, reason);
		for (const blocked of this.#blocked.values()) blocked.resolve();
		this.#blocked.clear();
		for (const waiter of this.#slotWaiters.splice(0)) waiter();
	}

	/** Resolve once every submitted op has been released or the pump has failed. */
	async drain(): Promise<void> {
		while (this.#running > 0 || this.#waiting.length > 0) {
			if (this.#failed) throw this.#failed;
			await new Promise<void>((resolve) => {
				this.#slotWaiters.push(resolve);
			});
		}
		if (this.#failed) throw this.#failed;
	}

	#kick(): void {
		while (this.#running < this.depth && this.#waiting.length > 0 && !this.#failed) {
			const job = this.#waiting.shift();
			if (!job) break;
			this.#running++;
			this.#active.push(job);
			void this.#run(job);
		}
	}

	#reject(job: { settled: boolean; reject: (reason: unknown) => void }, reason: Error): void {
		if (job.settled) return;
		job.settled = true;
		job.reject(reason);
	}

	async #run<T>(job: Job<T>): Promise<void> {
		try {
			if (this.#failed) {
				this.#reject(job, this.#failed);
				return;
			}
			const value = await job.start();
			await this.#waitTurn(job.id);
			if (this.#failed) {
				this.#reject(job, this.#failed);
				return;
			}
			if (job.settled) return;
			job.settled = true;
			job.resolve(value);
			this.#releaseNext();
		} catch (error) {
			const failure = error instanceof Error ? error : new Error(String(error));
			this.close(failure);
			this.#reject(job, failure);
		} finally {
			this.#running--;
			const i = this.#active.indexOf(job as unknown as Job<unknown>);
			if (i >= 0) this.#active.splice(i, 1);
			this.#kick();
			for (const waiter of this.#slotWaiters.splice(0)) waiter();
		}
	}

	// Hold this completion until every earlier submit has resolved, so callers that
	// write to the group after `await submit` cannot overtake an earlier frame.
	#waitTurn(id: number): Promise<void> {
		if (this.#failed) return Promise.reject(this.#failed);
		if (id === this.#nextRelease) return Promise.resolve();
		return new Promise<void>((resolve, reject) => {
			if (this.#failed) {
				reject(this.#failed);
				return;
			}
			this.#blocked.set(id, {
				resolve: () => {
					if (this.#failed) reject(this.#failed);
					else resolve();
				},
			});
		});
	}

	#releaseNext(): void {
		this.#nextRelease++;
		const pending = this.#blocked.get(this.#nextRelease);
		if (!pending) return;
		this.#blocked.delete(this.#nextRelease);
		pending.resolve();
	}
}

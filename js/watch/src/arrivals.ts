import type { Time } from "@moq/net";

/** One received frame, including a latency skip immediately before it. */
export interface Arrival {
	/** Media presentation time in milliseconds. */
	timestamp: Time.Milli;
	/** Monotonic transport ingress time in milliseconds. */
	arrival: Time.Milli;
	/** Whether ingress missed the playback deadline. */
	late: boolean;
	/** Whether latency enforcement skipped content before this frame. */
	skipped: boolean;
}

interface State {
	queue: Arrival[];
	// The next unread index. Advancing it instead of shifting keeps a long backlog O(1) per read.
	head: number;
	closed: boolean;
	wake?: () => void;
}

/**
 * Every frame a decoder receives while this handle is open, in delivery order.
 *
 * Unread entries queue until {@link next} takes them, so read continuously or close the handle;
 * closing releases the queue. Obtain one from a decoder's `arrivals()`.
 */
export class Arrivals implements AsyncIterable<Arrival> {
	readonly #state: State;
	readonly #open: Set<State>;
	#reading = false;

	/** @internal */
	constructor(state: State, open: Set<State>) {
		this.#state = state;
		this.#open = open;
	}

	/** Get the next received frame, or `undefined` once the handle or its decoder closes. */
	async next(): Promise<Arrival | undefined> {
		if (this.#reading) throw new Error("multiple calls to next not supported");
		this.#reading = true;
		const state = this.#state;
		try {
			for (;;) {
				const entry = state.queue[state.head];
				if (entry) {
					state.head++;
					// Drop the read prefix once it outweighs the rest, which amortizes to O(1).
					if (state.head * 2 >= state.queue.length) {
						state.queue.splice(0, state.head);
						state.head = 0;
					}
					return entry;
				}
				if (state.closed) return undefined;
				await new Promise<void>((resolve) => {
					state.wake = resolve;
				});
				state.wake = undefined;
			}
		} finally {
			this.#reading = false;
		}
	}

	/** Iterate received frames until the handle or its decoder closes; leaving the loop closes the handle. */
	async *[Symbol.asyncIterator](): AsyncIterator<Arrival> {
		try {
			for (;;) {
				const entry = await this.next();
				if (entry === undefined) return;
				yield entry;
			}
		} finally {
			this.close();
		}
	}

	/** Stop observing and release unread entries. */
	close(): void {
		this.#open.delete(this.#state);
		this.#state.queue.length = 0;
		this.#state.head = 0;
		this.#state.closed = true;
		this.#state.wake?.();
	}
}

/** Fans each received frame out to the open {@link Arrivals} handles. @internal */
export class Producer {
	readonly #open = new Set<State>();
	#closed = false;

	/** Open a handle that receives every later frame. */
	consume(): Arrivals {
		const state: State = { queue: [], head: 0, closed: this.#closed };
		if (!this.#closed) this.#open.add(state);
		return new Arrivals(state, this.#open);
	}

	write(arrival: Arrival): void {
		for (const state of this.#open) {
			state.queue.push(arrival);
			state.wake?.();
		}
	}

	/** End every handle once it drains what it already received. */
	close(): void {
		this.#closed = true;
		for (const state of this.#open) {
			state.closed = true;
			state.wake?.();
		}
		this.#open.clear();
	}
}

import { Signal } from "@moq/signals";
import * as Path from "./path.js";

/**
 * The availability of a broadcast.
 *
 * @public
 */
export interface Event {
	/** Broadcast path. */
	path: Path.Valid;
	/** True when the broadcast is available. */
	active: boolean;
}

/** Reactive backing state shared by announcement producers and consumers. */
class AnnounceState {
	queue = new Signal<Event[]>([]);
	closed = new Signal<boolean | Error>(false);
}

function closedPromise(state: AnnounceState): Promise<Error | undefined> {
	return new Promise((resolve) => {
		const dispose = state.closed.subscribe((closed) => {
			if (!closed) return;
			resolve(closed instanceof Error ? closed : undefined);
			dispose();
		});
	});
}

/**
 * The write side of an announcement stream.
 *
 * @public
 */
export class Producer {
	/** Path prefix this stream is scoped to. */
	prefix: Path.Valid;

	#state = new AnnounceState();

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor(prefix = Path.empty()) {
		this.prefix = prefix;
		this.closed = closedPromise(this.#state);
	}

	/** A read handle for this announcement stream. */
	consume(): Consumer {
		return new Consumer(this.prefix, this.#state as never);
	}

	/** Writes an announcement to the queue. */
	append(event: Event) {
		if (this.#state.closed.peek()) throw new Error("announcements are closed");
		this.#state.queue.mutate((queue) => {
			queue.push(event);
		});
	}

	/** Closes the writer. */
	close(abort?: Error) {
		this.#state.closed.set(abort ?? true);
		this.#state.queue.mutate((queue) => {
			queue.length = 0;
		});
	}
}

/**
 * The read side of an announcement stream.
 *
 * @public
 */
export class Consumer {
	/** Path prefix this stream is scoped to. */
	prefix: Path.Valid;

	#state: AnnounceState;

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor(prefix?: Path.Valid, state?: never);
	constructor(prefix = Path.empty(), state?: AnnounceState) {
		this.prefix = prefix;
		this.#state = state ?? new AnnounceState();
		this.closed = closedPromise(this.#state);
	}

	/** Returns the next announcement. */
	async next(): Promise<Event | undefined> {
		for (;;) {
			const announce = this.#state.queue.peek().shift();
			if (announce) return announce;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await Signal.race(this.#state.queue, this.#state.closed);
		}
	}

	/** Closes the reader. */
	close(abort?: Error) {
		this.#state.closed.set(abort ?? true);
		this.#state.queue.mutate((queue) => {
			queue.length = 0;
		});
	}
}

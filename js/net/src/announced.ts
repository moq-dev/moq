/**
 * Broadcast announcement streams: which broadcast paths are available under a prefix.
 *
 * @module
 */
import { Signal } from "@moq/signals";
import * as Path from "./path.js";

/**
 * The availability of a broadcast.
 *
 * @public
 */
export interface Event {
	/** Broadcast path relative to the prefix passed to `announced()`. */
	path: Path.Valid;
	/** True when the broadcast is available, false when it was removed. */
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
		return makeConsumer(this.prefix, this.#state);
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

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported AnnounceState. Assigned in the class's static block.
let makeConsumer: (prefix: Path.Valid, state: AnnounceState) => Consumer;

/**
 * The read side of an announcement stream.
 *
 * Created internally: obtain one from {@link Producer.consume} or the connection's
 * `announced(prefix)`.
 *
 * @public
 */
export class Consumer {
	/** Path prefix this stream is scoped to. */
	prefix: Path.Valid;

	#state: AnnounceState;

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	private constructor(prefix: Path.Valid, state: AnnounceState) {
		this.prefix = prefix;
		this.#state = state;
		this.closed = closedPromise(this.#state);
	}

	static {
		makeConsumer = (prefix, state) => new Consumer(prefix, state);
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

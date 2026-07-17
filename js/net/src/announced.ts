/**
 * Broadcast announcement streams: which broadcast paths are available under a prefix.
 *
 * @module
 */
import { type GetPromise, Once, Signal } from "@moq/signals";
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
	closed = new Once<Error | null>();
}

// Once.set throws on a second settle, and both ends of a stream can close independently.
function closeState(state: AnnounceState, abort?: Error) {
	if (state.closed.peek() !== undefined) return;
	state.closed.set(abort ?? null);
	state.queue.mutate((queue) => {
		queue.length = 0;
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

	constructor(prefix = Path.empty()) {
		this.prefix = prefix;
	}

	/**
	 * Settles once the stream closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** A read handle for this announcement stream. */
	consume(): Consumer {
		return makeConsumer(this.prefix, this.#state);
	}

	/** Writes an announcement to the queue. */
	append(event: Event) {
		if (this.#state.closed.peek() !== undefined) throw new Error("announcements are closed");
		this.#state.queue.mutate((queue) => {
			queue.push(event);
		});
	}

	/** Closes the writer. Idempotent. */
	close(abort?: Error) {
		closeState(this.#state, abort);
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

	private constructor(prefix: Path.Valid, state: AnnounceState) {
		this.prefix = prefix;
		this.#state = state;
	}

	/** Settles once the stream closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
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
			if (closed !== undefined) return undefined;

			await Signal.race(this.#state.queue, this.#state.closed);
		}
	}

	/** Closes the reader. Idempotent. */
	close(abort?: Error) {
		closeState(this.#state, abort);
	}
}

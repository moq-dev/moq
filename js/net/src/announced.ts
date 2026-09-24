/**
 * Broadcast announcement streams: which broadcast paths are available under a scope.
 *
 * @module
 */
import { type GetPromise, Once, Signal } from "@moq/signals";
import type { Route } from "./hop.js";
import type * as Path from "./path.js";

/**
 * What an {@link Update} reports about its prefix.
 *
 * @public
 */
export type Kind = "announced" | "updated" | "retracted";

/**
 * A route announcement, update, or retraction.
 *
 * An announcement is always a prefix, never a broadcast: a route claims that
 * {@link prefix} and every path beneath it can be served. By convention a publisher
 * announces each broadcast's exact path, so enumerating routes enumerates broadcasts;
 * resolve one with the origin's `request(path)`. Narrow with a {@link Path.Pattern}
 * locally to follow a subset.
 *
 * @public
 */
export interface Update {
	/**
	 * The prefix the route covers, relative to the origin (for a session, its URL path).
	 */
	prefix: Path.Valid;
	/** What the filter's wildcards stood for, when this prefix pins all of them. */
	captures: Path.Pattern[] | undefined;
	/** Whether the prefix was announced, re-priced, or retracted. */
	kind: Kind;
	/** Hops and cost of the route; on a retraction, its last advertised values. */
	route: Route;
}

/**
 * Options for an announcement stream.
 *
 * @public
 */
export interface Options {
	/**
	 * Also report hidden routes: those with a path segment starting with `.` below the
	 * scope's literal head. Hidden routes are left out by default, so a platform can add
	 * `.`-named broadcasts (stats, internal routes) without them turning up in apps that
	 * list everything. Subscribing to a hidden path by name works either way.
	 */
	hidden?: boolean;
}

/** Whether a route covers the path after an update of this {@link Kind}. */
export function isActive(kind: Kind): boolean {
	return kind !== "retracted";
}

/** Reactive backing state shared by announcement producers and consumers. */
class AnnounceState {
	queue = new Signal<Update[]>([]);
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
	#state = new AnnounceState();

	/**
	 * Settles once the stream closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** A read handle for this announcement stream. */
	consume(): Consumer {
		return makeConsumer(this.#state);
	}

	/** Writes an announcement to the queue. */
	append(update: Update) {
		if (this.#state.closed.peek() !== undefined) throw new Error("announcements are closed");
		this.#state.queue.mutate((queue) => {
			queue.push(update);
		});
	}

	/** Closes the writer. Idempotent. */
	close(abort?: Error) {
		closeState(this.#state, abort);
	}
}

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported AnnounceState. Assigned in the class's static block.
let makeConsumer: (state: AnnounceState) => Consumer;

/**
 * The read side of an announcement stream.
 *
 * Created internally: obtain one from {@link Producer.consume} or the connection's
 * `announced(scope)`.
 *
 * @public
 */
export class Consumer {
	#state: AnnounceState;

	private constructor(state: AnnounceState) {
		this.#state = state;
	}

	/** Settles once the stream closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	static {
		makeConsumer = (state) => new Consumer(state);
	}

	/** The announcements as they arrive, until the stream closes. */
	async *[Symbol.asyncIterator](): AsyncGenerator<Update, void, undefined> {
		for (;;) {
			const update = await this.next();
			if (!update) return;
			yield update;
		}
	}

	/** Returns the next announcement. */
	async next(): Promise<Update | undefined> {
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

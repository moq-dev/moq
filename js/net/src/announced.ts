/**
 * Broadcast announcement streams: which broadcast paths are available under a scope.
 *
 * @module
 */
import { type GetPromise, Once, Signal } from "@moq/signals";
import type { Route } from "./hop.js";
import type * as Path from "./path.js";

/**
 * A route over a prefix, delivered inside an {@link Event}.
 *
 * An announcement is always a prefix, never a broadcast: a route claims that
 * {@link prefix} and every path beneath it can be served. By convention a publisher
 * announces each broadcast's exact path, so enumerating routes enumerates broadcasts;
 * resolve one with the origin's `request(path)`. Narrow with a {@link Path.Pattern}
 * locally to follow a subset.
 *
 * @public
 */
export interface Announce {
	/**
	 * The prefix the route covers, relative to the origin (for a session, its URL path).
	 */
	prefix: Path.Valid;
	/** What the filter's wildcards stood for, when this prefix pins all of them. */
	captures: Path.Pattern[] | undefined;
	/** Hops and cost of the route; on a retraction, its last advertised values. */
	route: Route;
}

/**
 * What an announcement stream yields.
 *
 * `announced`: a route now covers the prefix. `updated`: the route covering it changed
 * hops or cost, in place. `retracted`: no route covers it any more. `live`: every route
 * live at subscribe time has been delivered, including those a connected peer was still
 * sending, so what follows is live changes; yielded at most once, and a caller listing
 * what is live stops there.
 *
 * @public
 */
export type Event = ({ kind: "announced" | "updated" | "retracted" } & Announce) | { kind: "live" };

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

/** Reactive backing state shared by announcement producers and consumers. */
class AnnounceState {
	queue = new Signal<Event[]>([]);
	// Whether the live marker was appended, so a repeat (from a stream spanning sessions) is dropped.
	live = false;
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

	/** Writes an event to the queue. The `live` marker is written once; later ones are dropped. */
	append(event: Event) {
		if (this.#state.closed.peek() !== undefined) throw new Error("announcements are closed");
		if (event.kind === "live") {
			if (this.#state.live) return;
			this.#state.live = true;
		}
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

	/** The events as they arrive, until the stream closes. */
	async *[Symbol.asyncIterator](): AsyncGenerator<Event, void, undefined> {
		for (;;) {
			const event = await this.next();
			if (!event) return;
			yield event;
		}
	}

	/** Returns the next event, or undefined once the stream closes. */
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

/**
 * When an initial set has landed, for wires that never say where it ends (lite-03/04,
 * moq-transport): once its stream goes quiet.
 *
 * A peer writes its whole set back to back, so the first announcement gets a round trip's
 * grace and each one after it only has to beat its siblings. Mirrors `Quiet` in `rs/moq-net`.
 *
 * @internal
 */
export class Quiet {
	/** How long the stream may stay silent before its first announcement, in ms. */
	static readonly FIRST = 500;
	/** How long the stream may stay silent between announcements, in ms. */
	static readonly GAP = 30;

	#landed: () => void;
	#timer: ReturnType<typeof setTimeout> | undefined;

	/** Start counting from now; `landed` runs once the stream goes quiet. */
	constructor(landed: () => void) {
		this.#landed = landed;
		this.#timer = setTimeout(() => this.#land(), Quiet.FIRST);
	}

	/** An announcement arrived: the set is still landing. */
	heard(): void {
		if (this.#timer === undefined) return;
		clearTimeout(this.#timer);
		this.#timer = setTimeout(() => this.#land(), Quiet.GAP);
	}

	/** Stop counting without landing, once the stream is gone. Idempotent. */
	close(): void {
		clearTimeout(this.#timer);
		this.#timer = undefined;
	}

	#land(): void {
		this.#timer = undefined;
		this.#landed();
	}
}

/**
 * Broadcast announcement streams: which broadcast paths are available under a scope.
 *
 * @module
 */
import { type GetPromise, type Getter, Once, Signal } from "@moq/signals";
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
 * `start`: a route now covers the prefix. `update`: the route covering it changed
 * hops or cost, in place. `restart`: another publisher instance now serves the prefix (a
 * newer epoch, or another route without one), so request it afresh; what was already
 * resolved stays on the old one until dropped or its route goes. `end`: no route covers it
 * any more.
 *
 * @public
 */
export type Event = { kind: "start" | "update" | "restart" | "end" } & Announce;

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

	/**
	 * Also report every route that could serve a path in scope, not just those whose literal
	 * name matches it. A route is a prefix serving everything beneath it, so a scope listing
	 * `.chat` names at any depth still reports a route at `room`, which can serve
	 * `room/lobby.chat`. Code that routes or forwards needs these; listing broadcasts does
	 * not. Requests resolve through covering routes either way.
	 */
	covering?: boolean;
}

/** Reactive backing state shared by announcement producers and consumers. */
class AnnounceState {
	queue = new Signal<Event[]>([]);
	closed = new Once<Error | null>();
	synced = new Signal(false);
	// The queued events still to read before the stream counts as synced, or undefined
	// while the initial set is still arriving.
	owed: number | undefined;
	// A reader holds the last event it took before syncing until it asks for the next
	// one, so whatever it does with that event lands before synced flips.
	reading = false;
	// Open consumer handles; the stream is wanted while any is.
	consumers = 0;
	wanted = new Signal(false);
}

// Synced once nothing is owed and the reader has come back for more.
function settle(state: AnnounceState) {
	if (state.owed === 0 && !state.reading) state.synced.set(true);
}

// Once.set throws on a second settle, and both ends of a stream can close independently.
function closeState(state: AnnounceState, abort?: Error) {
	if (state.closed.peek() !== undefined) return;
	state.closed.set(abort ?? null);
	state.queue.mutate((queue) => {
		queue.length = 0;
	});
	// Nothing more is coming, so nothing waiting on the boundary should hang.
	state.owed = 0;
	state.reading = false;
	state.synced.set(true);
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

	/**
	 * Whether any consumer still holds the stream: false before the first {@link consume} and
	 * once the last one {@link Consumer.close}s, which is how a reader cancels it.
	 */
	get wanted(): Getter<boolean> {
		return this.#state.wanted;
	}

	/** Writes an event to the queue. */
	append(event: Event) {
		if (this.#state.closed.peek() !== undefined) throw new Error("announcements are closed");
		this.#state.queue.mutate((queue) => {
			queue.push(event);
		});
	}

	/**
	 * Marks the initial set as delivered: the stream syncs once the reader has taken
	 * everything queued so far. A no-op until {@link behind} once marked.
	 *
	 * @internal
	 */
	synced() {
		const state = this.#state;
		if (state.owed !== undefined) return;
		state.owed = state.queue.peek().length;
		settle(state);
	}

	/**
	 * Takes sync back until the next {@link synced}, as when another initial set starts.
	 *
	 * @internal
	 */
	behind() {
		const state = this.#state;
		if (state.closed.peek() !== undefined) return;
		state.owed = undefined;
		state.synced.set(false);
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
 * Created internally: obtain one from an origin or the connection's `announced(scope)`.
 *
 * @public
 */
export class Consumer {
	#state: AnnounceState;
	// Set once this handle closes, ending its own reads and guarding the consumer count.
	#released = new Signal(false);

	private constructor(state: AnnounceState) {
		this.#state = state;
		state.consumers++;
		state.wanted.set(true);
	}

	/** Settles once the stream closes: `null` on a clean close, or the abort {@link Error}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	static {
		makeConsumer = (state) => new Consumer(state);
	}

	/**
	 * Whether the stream has delivered its source's initial announcements and the reader has
	 * taken them, so a prefix not announced by now is absent rather than not yet heard of.
	 *
	 * Flips once the reader asks for the event after the last of them, and is true once
	 * closed. Through a session, the initial set is the peer's view when it answered: exact
	 * on versions that count it, best-effort on the rest. Reading an origin, it is every
	 * attached session's initial set, so a reconnect takes it back until the new one lands.
	 */
	get synced(): Getter<boolean> {
		return this.#state.synced;
	}

	/** The events as they arrive, until the stream closes. */
	async *[Symbol.asyncIterator](): AsyncGenerator<Event, void, undefined> {
		for (;;) {
			const event = await this.next();
			if (!event) return;
			yield event;
		}
	}

	/** Returns the next event, or undefined once the stream or this handle closes. */
	async next(): Promise<Event | undefined> {
		const state = this.#state;
		state.reading = false;
		settle(state);
		for (;;) {
			if (this.#released.peek()) return undefined;
			const announce = state.queue.peek().shift();
			if (announce) {
				if (state.owed) state.owed -= 1;
				if (!state.synced.peek()) state.reading = true;
				return announce;
			}

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return undefined;

			await Signal.race(state.queue, state.closed, this.#released);
		}
	}

	/**
	 * Release this handle, like dropping a Rust handle: other consumers keep reading, and the
	 * producer sees the stream as no longer wanted once the last one closes.
	 * Idempotent.
	 */
	close() {
		if (this.#released.peek()) return;
		this.#released.set(true);
		if (--this.#state.consumers === 0) this.#state.wanted.set(false);
	}
}

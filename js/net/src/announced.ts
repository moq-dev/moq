/**
 * Broadcast announcement streams: which broadcast paths are available under a scope.
 *
 * @module
 */
import { type GetPromise, Once, Signal } from "@moq/signals";
import type { Route } from "./hop.js";
import * as Path from "./path.js";

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

	/** Writes an event to the queue. */
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
 * A reducer from the announcements of the routes covering `path` to those of the one route
 * serving it: the most specific. Another route taking over is a `restart`, or an `update` when
 * both carry the same epoch, since those serve the same bytes. Routes beneath `path` serve
 * other broadcasts and are skipped, as is any change no request sees.
 *
 * While nothing serves the path, the rest of a batch (one change to the table) folds into one
 * `start`, so the replay a late follower begins with (a covering prefix, then the exact path
 * beneath it) starts on the route serving the path rather than starting and restarting. Once
 * something serves it, each change comes through on its own: an `end` followed by a `start` is
 * a gap that ended any request on the old route, even when both routes carry one epoch.
 * Synchronous, so a followed stream delivers in step with an announced one. Mirrors
 * `announce::Follow` in rs/moq-net.
 *
 * @internal
 */
export function follower(path: Path.Valid): (events: Event[]) => Event[] {
	// Every route standing over the path, by prefix.
	const covering = new Map<Path.Valid, Announce>();
	const serving = () => {
		let best: Announce | undefined;
		for (const announce of covering.values()) {
			if (!best || announce.prefix.length > best.prefix.length) best = announce;
		}
		return best;
	};

	// Apply one route's event, returning what it means for the path alone.
	const fold = ({ kind, ...announce }: Event): Event | undefined => {
		if (!Path.hasPrefix(announce.prefix, path)) return undefined;
		const before = serving();
		if (kind === "end") covering.delete(announce.prefix);
		else covering.set(announce.prefix, announce);
		const after = serving();

		if (!before) return after && { kind: "start", ...after };
		if (!after) return { kind: "end", ...before };
		if (before.prefix !== after.prefix) {
			const same = before.route.epoch !== undefined && before.route.epoch === after.route.epoch;
			return { kind: same ? "update" : "restart", ...after };
		}
		// A route less specific than the serving one changing is skipped: no request sees it.
		if (after.prefix !== announce.prefix) return undefined;
		return { kind: kind === "restart" ? "restart" : "update", ...after };
	};

	return (events) => {
		const reduced: Event[] = [];
		let idle = !serving();
		for (const event of events) {
			const change = fold(event);
			if (idle || !change) continue;
			reduced.push(change);
			idle = change.kind === "end";
		}
		const after = serving();
		if (idle && after) reduced.push({ kind: "start", ...after });
		return reduced;
	};
}

/**
 * A broadcast routing table, independent of any connection.
 *
 * Publish broadcasts into an origin and hand the origin to one or more connections to
 * serve them; the broadcasts outlive any single session. Hand the same (or another)
 * origin to a connection's `subscribe` option and the peer's announced routes appear
 * in the table too: each route covers a path prefix, and a request for a path under
 * it resolves through the session that announced it. Mirrors the `origin` module in
 * `rs/moq-net`.
 *
 * @module
 */
import { Derived, type Dispose, type GetPromise, type Getter, getter, Once, Signal } from "@moq/signals";
import * as announce from "./announced.ts";
import * as broadcast from "./broadcast.ts";
import { StreamCode, StreamError } from "./error.ts";
import { DEFAULT_ROUTE, normalizeRoute, type Route, routesEqual } from "./hop.ts";
import { hooks } from "./internal.ts";
import * as Path from "./path.ts";

export type { Cost, Hop, Route } from "./hop.ts";
export { DEFAULT_ROUTE, normalizeRoute, ZERO_COST } from "./hop.ts";

/**
 * One requested path: the notify node for everything watching it.
 *
 * `route` is the only reactive part, and the only thing a {@link Request} subscribes to, so
 * a publish or retraction anywhere else in the table cannot wake it. The origin's tables stay
 * the storage; this is a per-path view onto them, refreshed by whichever mutator touched the
 * path. The alternative, deriving each request over the whole `local`/`remote` maps, wakes
 * every open request on every unrelated change.
 *
 * `answer` outlives a session: the answering session clears it when it dies and the next one
 * answers again, which is what makes a request span reconnects.
 *
 * @internal
 */
export interface RequestSlot {
	count: number;
	answer?: broadcast.Consumer;
	readonly route: Signal<broadcast.Consumer | undefined>;
}

/**
 * One advertised prefix: hops and cost, plus an optional server that answers
 * requests beneath it.
 *
 * Newest entry per prefix is the one requests resolve through. An originated
 * entry is forwarded by sessions; a received one is not, so a shared origin
 * cannot echo a peer's announcements back to it.
 */
interface RouteEntry {
	readonly identity: object;
	readonly route: Signal<Route>;
	readonly originated: boolean;
	readonly server?: ServeState;
}

/** A served route from {@link Producer.dynamic}: the queue a handler drains. */
class ServeState {
	queue = new Signal<BroadcastRequest[]>([]);
	pending = new Map<Path.Valid, BroadcastRequest>();
	served = new Map<Path.Valid, broadcast.Consumer>();
	closed = new Once<Error | null>();
	settled = new Signal(0);
	onChange: (path: Path.Valid) => void = () => {};

	enqueue(path: Path.Valid): void {
		if (this.closed.peek() !== undefined) return;
		if (this.pending.has(path)) return;
		const live = this.served.get(path);
		if (live && live.closed.peek() === undefined) return;
		const request = makeBroadcastRequest(path, this);
		this.pending.set(path, request);
		this.queue.mutate((queue) => {
			queue.push(request);
		});
	}

	accept(request: BroadcastRequest, front: broadcast.Consumer): void {
		if (this.closed.peek() !== undefined || this.pending.get(request.path) !== request) {
			front.close();
			return;
		}
		this.pending.delete(request.path);
		const existing = this.served.get(request.path);
		if (existing && existing.closed.peek() === undefined) {
			front.close();
			this.onChange(request.path);
			this.settled.update((n) => n + 1);
			return;
		}
		this.served.set(request.path, front);
		void front.closed.then(() => {
			if (this.served.get(request.path) === front) this.served.delete(request.path);
		});
		this.onChange(request.path);
		this.settled.update((n) => n + 1);
	}

	reject(request: BroadcastRequest, _err: Error): void {
		if (this.pending.get(request.path) !== request) return;
		this.pending.delete(request.path);
		this.onChange(request.path);
		this.settled.update((n) => n + 1);
	}

	close(abort?: Error): void {
		if (this.closed.peek() !== undefined) return;
		const err = abort ?? new StreamError(StreamCode.NoCapacity, { message: "no capacity" });
		this.closed.set(err);
		const queued = [...this.pending.values()];
		this.pending.clear();
		this.queue.mutate((queue) => {
			queue.length = 0;
		});
		for (const request of queued) {
			finishBroadcastRequest(request, err);
		}
		for (const [path, front] of this.served) {
			front.close(abort);
			this.onChange(path);
		}
		this.served.clear();
		this.settled.update((n) => n + 1);
	}
}

/** Publisher-facing advertisement: object identity plus the current route. */
export interface Advertised {
	/** A republish is a different object; a re-price is the same object with a new route. */
	readonly identity: object;
	readonly route: Route;
}

/** Reactive backing state shared by origin producers and consumers. */
class OriginState {
	// Both tables decouple the application producing into the origin from the
	// connections serving or feeding it. Undefined once the origin closes, so late
	// writes fail loudly.
	//
	// Local is what this endpoint creates, keyed by exact path: reachable for
	// subscribes whether or not it is advertised. Routes is the advertisement table:
	// prefixes a dynamic handle or a received session covers, newest first. They stay
	// separate so a session can never announce a received entry back to a peer, which
	// is what makes an origin shared by both directions echo-free.
	local = new Signal<Map<Path.Valid, broadcast.Consumer> | undefined>(new Map());
	advertisedLocal = new Signal<Map<Path.Valid, Route> | undefined>(new Map());
	routes = new Signal<Map<Path.Valid, RouteEntry[]> | undefined>(new Map());

	// Originated advertisements sessions should forward: exact-path announces plus
	// originated dynamics. Identity is the local front or the route entry, so a
	// republish diffs as retract-then-announce and a re-price as another active.
	originated = new Signal<Map<Path.Valid, Advertised> | undefined>(new Map());

	// Broadcasts materialized from a served route, keyed by exact path. Shared by every
	// request for the path so repeats reuse one accept; dropped (and closed) when the
	// providing route goes away or the last request releases it.
	materialized = new Map<Path.Valid, { entry: RouteEntry; front: broadcast.Consumer }>();

	// Paths consumers asked for without waiting for an announcement; attached sessions
	// answer them with blind subscriptions. Never announced: an answered request is assumed
	// present, not known live, so it must not read as an availability claim.
	requests = new Signal<Map<Path.Valid, RequestSlot> | undefined>(new Map());

	// How many sessions are attached, and how many of those support broadcast discovery.
	// What backs the public `discovery` getter.
	sessions = new Signal({ total: 0, discovery: 0 });

	// How many things are prepared to answer a request: attached sessions, plus reconnecting
	// connections that have no session right now but will. Zero means an unrouted path is
	// unroutable rather than merely unanswered, which is the whole difference between "wait,
	// this is coming" and "nothing here can ever serve you".
	answerers = new Signal(0);

	closed = new Once<Error | null>();

	/**
	 * Recompute what `path` resolves to, waking only the requests watching that path.
	 *
	 * A no-op for a path nobody requested, so the common case (publishing into a table
	 * nobody is asking about) costs a map lookup. Call after any write that could change
	 * the answer for a single path.
	 */
	refresh(path: Path.Valid): void {
		const slot = this.requests.peek()?.get(path);
		if (!slot) return;
		slot.route.set(this.route(path, slot.answer));
	}

	/**
	 * Recompute every open request covered by `prefix`, after a route was inserted or
	 * removed there: a route covers many paths, so a single-path refresh is not enough.
	 * Materialized broadcasts whose provider changed are released here too, so a
	 * retracted route's session subscription closes even when nothing reads it again.
	 */
	refreshPrefix(prefix: Path.Valid): void {
		for (const [path, cached] of [...this.materialized]) {
			if (!Path.hasPrefix(prefix, path)) continue;
			if (cached.entry !== this.bestEntry(path)) {
				this.materialized.delete(path);
				cached.front.close();
			}
		}
		for (const [path, slot] of this.requests.peek() ?? []) {
			if (Path.hasPrefix(prefix, path)) slot.route.set(this.route(path, slot.answer));
		}
	}

	/** Rebuild the publisher-facing originated table after an advertisement write. */
	rebuildOriginated(): void {
		const local = this.local.peek();
		const advertised = this.advertisedLocal.peek();
		const routes = this.routes.peek();
		if (!local && !advertised && !routes) {
			this.originated.set(undefined);
			return;
		}
		const next = new Map<Path.Valid, Advertised>();
		for (const [path, route] of advertised ?? []) {
			const front = local?.get(path);
			if (front) next.set(path, { identity: front, route });
		}
		for (const [prefix, entries] of routes ?? []) {
			const mine = entries.find((entry) => entry.originated);
			if (mine) next.set(prefix, { identity: mine.identity, route: mine.route.peek() });
		}
		this.originated.set(next);
	}

	/**
	 * Release the materialized broadcast for `path`, once its last request is gone: the
	 * cache exists to share one session subscription between requests, not to outlive
	 * them.
	 */
	releaseMaterialized(path: Path.Valid): void {
		const cached = this.materialized.get(path);
		if (!cached) return;
		this.materialized.delete(path);
		cached.front.close();
	}

	/** The newest entry on the most specific route covering `path`, if any. */
	bestEntry(path: Path.Valid): RouteEntry | undefined {
		let bestPrefix: Path.Valid | undefined;
		let best: RouteEntry | undefined;
		for (const [prefix, entries] of this.routes.peek() ?? []) {
			if (!entries[0]) continue;
			if (!Path.hasPrefix(prefix, path)) continue;
			if (bestPrefix === undefined || prefix.length > bestPrefix.length) {
				bestPrefix = prefix;
				best = entries[0];
			}
		}
		return best;
	}

	/**
	 * What `path` resolves to: a local publish, a broadcast materialized from the best
	 * covering route, or the blind answer.
	 *
	 * Materialization is lazy and cached per path: the first request under a route opens
	 * the providing session's subscription, repeats share it, and a provider change (the
	 * route retracting, a better session taking over) swaps it out.
	 */
	route(path: Path.Valid, answer?: broadcast.Consumer): broadcast.Consumer | undefined {
		const local = this.local.peek()?.get(path);
		if (local) return local;

		const entry = this.bestEntry(path);
		const cached = this.materialized.get(path);
		if (cached && cached.entry === entry) return cached.front;
		if (cached) {
			this.materialized.delete(path);
			cached.front.close();
		}
		if (!entry?.server) return answer;

		const served = entry.server.served.get(path);
		if (served && served.closed.peek() === undefined) {
			this.materialized.set(path, { entry, front: served });
			return served;
		}

		entry.server.enqueue(path);
		return undefined;
	}
}

/**
 * A non-owning handle on an origin: publish into it and read it, without its lifecycle.
 *
 * What a shared connection lends out. {@link Producer} implements it, so code that is
 * handed an origin rather than owning one should accept this type: closing the origin
 * stays the owner's alone, and a borrower cannot express it.
 *
 * @public
 */
export interface Table {
	/** Settles once the origin closes; see {@link Producer.closed}. */
	readonly closed: GetPromise<Error | null>;

	/** Whether every attached session announces into the table; see {@link Consumer.discovery}. */
	readonly discovery: Getter<boolean | undefined>;

	/** Create an unadvertised broadcast at `path`; see {@link Producer.createBroadcast}. */
	createBroadcast(path: Path.Valid): broadcast.Producer;

	/** Resolve `path`, without waiting for an announcement; see {@link Consumer.request}. */
	request(path: Path.Valid): Request;

	/** The available broadcasts under `prefix`, as a live stream; see {@link Consumer.announced}. */
	announced(prefix?: Path.Valid): announce.Consumer;
}

/**
 * The write side of an origin: create broadcasts by path and advertise them.
 *
 * Independent of any connection. A connection given this origin (via its `publish` option)
 * announces and serves the table's originated advertisements for as long as the session
 * lasts; the broadcasts themselves live until their producer closes or {@link close} tears
 * the origin down. A reconnecting session re-announces the table on each attach, so
 * advertisements made while offline surface on the next connection.
 *
 * Create, attach {@link dynamic} for tracks served on demand, populate, then
 * {@link broadcast.Producer.announce}: an exact-path subscribe before the tracks exist is
 * refused, and announcing only makes a path discoverable.
 *
 * @public
 */
export class Producer implements Table {
	#state = new OriginState();

	// The reader backing the passthroughs, so holding a Producer never requires the
	// consume().x() stutter for everyday reads. One instance, so `discovery` keeps its
	// identity across reads.
	#reader = makeConsumer(this.#state);

	/**
	 * Settles once the origin closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/**
	 * Create a broadcast at `path`, returning its producer.
	 *
	 * The broadcast starts unadvertised: it is reachable by exact path for subscribes
	 * and fetches. Advertise it once its tracks exist with
	 * {@link broadcast.Producer.announce}; the two are independent, so cached or
	 * on-demand content can stay reachable without ever being announced.
	 *
	 * Close the producer to drop it. Creating a path again supersedes the previous
	 * broadcast: the origin drops its handle on the old one, which closes it unless the
	 * application still holds a consumer clone. A local broadcast also shadows any
	 * remote broadcast at the same path.
	 */
	createBroadcast(path: Path.Valid): broadcast.Producer {
		const producer = new broadcast.Producer();
		const front = producer.consume();

		hooks.attachAnnouncer(producer, {
			announce: (route) => this.#advertiseExact(path, front, route),
			unannounce: () => this.#retractExact(path, front),
		});

		this.#state.local.mutate((broadcasts) => {
			if (!broadcasts) throw new Error("origin is closed");
			broadcasts.get(path)?.close();
			broadcasts.set(path, front);
		});
		this.#state.advertisedLocal.mutate((advertised) => {
			advertised?.delete(path);
		});
		this.#state.rebuildOriginated();
		this.#state.refresh(path);

		// Drop it when the broadcast closes, unless a recreate already replaced it: a
		// stale broadcast closing must not unpublish the live one.
		void front.closed.then(() => {
			this.#retractExact(path, front);
			this.#state.local.mutate((broadcasts) => {
				if (broadcasts?.get(path) === front) broadcasts.delete(path);
			});
			this.#state.refresh(path);
		});

		return producer;
	}

	#advertiseExact(path: Path.Valid, front: broadcast.Consumer, route: Route): void {
		this.#state.advertisedLocal.mutate((advertised) => {
			if (!advertised) throw new Error("origin is closed");
			if (this.#state.local.peek()?.get(path) !== front) throw new Error("broadcast is closed");
			advertised.set(path, route);
		});
		this.#state.rebuildOriginated();
		this.#state.refresh(path);
	}

	#retractExact(path: Path.Valid, front: broadcast.Consumer): void {
		this.#state.advertisedLocal.mutate((advertised) => {
			if (this.#state.local.peek()?.get(path) !== front) return;
			advertised?.delete(path);
		});
		this.#state.rebuildOriginated();
		this.#state.refresh(path);
	}

	/**
	 * Advertise a path pattern and serve the requests beneath it.
	 *
	 * Until Advertise lands, only a prefix-shaped pattern is accepted (`foo/**`, or
	 * `**` for every path). The advertisement is visible to {@link Consumer.announced}
	 * and forwarded by sessions for as long as the returned {@link Dynamic} lives.
	 * A consumer resolving a path under the prefix that no local broadcast covers is
	 * handed to the handle as a {@link BroadcastRequest}.
	 */
	dynamic(
		pattern: Path.Pattern | string,
		route: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint } = DEFAULT_ROUTE,
	): Dynamic {
		return this.#insertRoute(pattern, normalizeRoute(route), true);
	}

	/**
	 * Land a route a peer announced, served through the returned handle. Same as
	 * {@link dynamic} but not originated, so a session never announces it back.
	 *
	 * @internal
	 */
	receive(
		pattern: Path.Pattern | string,
		route: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint } = DEFAULT_ROUTE,
	): Dynamic {
		return this.#insertRoute(pattern, normalizeRoute(route), false);
	}

	#insertRoute(pattern: Path.Pattern | string, route: Route, originated: boolean): Dynamic {
		const { parsed, prefix } = prefixPattern(pattern);
		const server = new ServeState();
		server.onChange = (path) => this.#state.refresh(path);
		const entry: RouteEntry = {
			identity: {},
			route: new Signal(route),
			originated,
			server,
		};

		let closed = false;
		this.#state.routes.mutate((routes) => {
			if (!routes) {
				closed = true;
				return;
			}
			const entries = routes.get(prefix);
			if (entries) entries.unshift(entry);
			else routes.set(prefix, [entry]);
		});
		if (closed) {
			server.close();
			return makeDynamic(parsed, prefix, entry, this.#state, () => {});
		}
		this.#state.rebuildOriginated();
		this.#state.refreshPrefix(prefix);

		const retract = () => {
			this.#state.routes.mutate((routes) => {
				const entries = routes?.get(prefix);
				if (!entries) return;
				const index = entries.indexOf(entry);
				if (index < 0) return;
				entries.splice(index, 1);
				if (entries.length === 0) routes?.delete(prefix);
			});
			server.close();
			this.#state.rebuildOriginated();
			this.#state.refreshPrefix(prefix);
		};

		return makeDynamic(parsed, prefix, entry, this.#state, retract);
	}

	/**
	 * Register an attached session, counting it toward the `discovery` state. Returns the
	 * detach; call it exactly once when the session dies.
	 *
	 * @internal
	 */
	attach(discovery: boolean): Dispose {
		this.#sessions(1, discovery);
		const release = this.expect();
		let detached = false;
		return () => {
			if (detached) return;
			detached = true;
			this.#sessions(-1, discovery);
			release();
		};
	}

	#sessions(delta: number, discovery: boolean): void {
		this.#state.sessions.update(({ total, discovery: d }) => ({
			total: total + delta,
			discovery: d + (discovery ? delta : 0),
		}));
	}

	/**
	 * Declare that something will answer requests on this origin, even with no session
	 * attached right now.
	 *
	 * A reconnecting connection holds one for its whole life, so a request made during a
	 * reconnect (or before the first session establishes) stays pending instead of reading as
	 * unroutable. Without it, {@link Request.unroutable} would fire on every page load, in the
	 * window between wiring the origin up and the handshake completing. Call the returned
	 * dispose when the connection is done for good.
	 *
	 * @internal
	 */
	expect(): Dispose {
		this.#state.answerers.update((count) => count + 1);
		let released = false;
		return () => {
			if (released) return;
			released = true;
			// Clamped because closing the origin zeroes the count, and the sessions attached at
			// the time still release afterwards.
			this.#state.answerers.update((count) => Math.max(0, count - 1));
		};
	}

	/**
	 * The open requests, watched by attached sessions to answer them; see
	 * {@link Consumer.request}. Undefined once the origin closes.
	 *
	 * @internal
	 */
	get requests(): Getter<ReadonlyMap<Path.Valid, RequestSlot> | undefined> {
		return this.#state.requests;
	}

	/**
	 * Resolves once anything a serving session scans changes: the open requests, or either
	 * side of the routing table.
	 *
	 * @internal
	 */
	changed(): Promise<unknown> {
		return Signal.race(this.#state.requests, this.#state.local, this.#state.routes, this.#state.advertisedLocal);
	}

	/**
	 * Provide `front` as the answer for the open request on `path`, taking ownership of it.
	 *
	 * Returns undefined (releasing the front) when the request is gone or already answered;
	 * first session in wins, and a loser must stay eligible to answer later. The returned
	 * withdraw releases the front and, if it was the standing answer, vacates the slot and
	 * wakes the other serving loops so a standby session answers immediately; call it when
	 * the session dies.
	 *
	 * @internal
	 */
	answer(path: Path.Valid, front: broadcast.Consumer): Dispose | undefined {
		const slot = this.#state.requests.peek()?.get(path);
		if (!slot || slot.answer !== undefined) {
			front.close();
			return undefined;
		}
		slot.answer = front;
		this.#state.refresh(path);

		return () => {
			if (slot.answer === front) {
				slot.answer = undefined;
				this.#state.refresh(path);
				// The route signal only reaches this path's requesters; poke the map so every
				// serving loop re-scans and one of them re-answers.
				this.#state.requests.mutate(() => {});
			}
			front.close();
		};
	}

	/** A read handle for this origin, the side a connection's `publish` option borrows. */
	consume(): Consumer {
		return makeConsumer(this.#state);
	}

	/** Whether every attached session announces into the table; see {@link Consumer.discovery}. */
	get discovery(): Getter<boolean | undefined> {
		return this.#reader.discovery;
	}

	/** Resolve `path`, without waiting for an announcement; see {@link Consumer.request}. */
	request(path: Path.Valid): Request {
		return this.#reader.request(path);
	}

	/** Whether the table routes `path` itself; see {@link Consumer.routes}. @internal */
	routes(path: Path.Valid): boolean {
		return this.#reader.routes(path);
	}

	/** The available broadcasts under `prefix`, as a live stream; see {@link Consumer.announced}. */
	announced(prefix?: Path.Valid): announce.Consumer {
		return this.#reader.announced(prefix);
	}

	/** Close the origin, every broadcast it still routes, and its announcement streams. Idempotent. */
	close(abort?: Error) {
		if (this.#state.closed.peek() !== undefined) return;
		this.#state.closed.set(abort ?? null);
		this.#state.local.update((broadcasts) => {
			for (const front of broadcasts?.values() ?? []) {
				front.close(abort);
			}
			return undefined;
		});
		this.#state.advertisedLocal.update(() => undefined);
		this.#state.routes.update((routes) => {
			for (const entries of routes?.values() ?? []) {
				for (const entry of entries) entry.server?.close(abort);
			}
			return undefined;
		});
		this.#state.originated.update(() => undefined);
		// Materialized broadcasts are handles we opened; release them.
		for (const cached of this.#state.materialized.values()) {
			cached.front.close();
		}
		this.#state.materialized.clear();
		// Nothing will answer a request on a closed origin, whatever is still attached, so
		// existing requests report unroutable rather than waiting on a corpse.
		this.#state.answerers.set(0);
		this.#state.requests.update((requests) => {
			for (const slot of requests?.values() ?? []) {
				slot.answer?.close();
				slot.answer = undefined;
				slot.route.set(undefined);
			}
			return undefined;
		});
	}
}

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported OriginState. Assigned in the class's static block.
let makeConsumer: (state: OriginState) => Consumer;

// Same for Request: a public constructor would let a caller forge a handle that no origin
// ever registered, whose lifecycle guarantees are then false. `@internal` alone would not
// stop it, since the declaration emit keeps the constructor.
let makeRequest: (
	path: Path.Valid,
	active: Getter<broadcast.Consumer | undefined>,
	unroutable: Getter<boolean>,
	dispose: Dispose,
) => Request;

let makeDynamic: (
	pattern: Path.Pattern,
	prefix: Path.Valid,
	entry: RouteEntry,
	state: OriginState,
	retract: Dispose,
) => Dynamic;

let makeBroadcastRequest: (path: Path.Valid, server: ServeState) => BroadcastRequest;
let finishBroadcastRequest: (request: BroadcastRequest, err: Error) => void;

function prefixPattern(pattern: Path.Pattern | string): { parsed: Path.Pattern; prefix: Path.Valid } {
	const parsed = typeof pattern === "string" ? Path.Pattern.parse(pattern) : pattern;
	const segments = parsed.segments;
	const last = segments[segments.length - 1];
	if (last?.kind !== "globstar") {
		throw new Error(
			`pattern ${parsed.text} is not a prefix; only a prefix (foo/**) is accepted until Advertise lands`,
		);
	}
	for (let i = 0; i < segments.length - 1; i++) {
		if (segments[i].kind !== "literal") {
			throw new Error(
				`pattern ${parsed.text} is not a prefix; only a prefix (foo/**) is accepted until Advertise lands`,
			);
		}
	}
	return { parsed, prefix: Path.from(parsed.head) };
}

/**
 * An open request for a path nothing announced; see {@link Consumer.request}.
 *
 * @public
 */
export class Request {
	/** The requested path. */
	readonly path: Path.Valid;

	/**
	 * The resolved broadcast, or undefined while nothing provides the path.
	 *
	 * The table's route when it has one: a local publish (no round trip) or an announced
	 * broadcast, swapping when a republish takes the path. Otherwise a session's blind
	 * answer, which is assumed present rather than known live: a missing broadcast
	 * surfaces as a reset on the first track subscription, not here. Drops back to
	 * undefined when the providing route dies and resolves again when another appears.
	 *
	 * Yours for as long as the request is open: it is a handle of this request's own, so
	 * closing it ends your view of the path rather than the route everyone else reads.
	 * {@link close} releases whatever is current.
	 */
	readonly active: Getter<broadcast.Consumer | undefined>;

	/**
	 * Whether nothing can serve this path, as opposed to not having served it yet.
	 *
	 * True when the origin routes nothing here and nothing is prepared to answer: no session
	 * attached and no connection reconnecting toward one. False whenever {@link active} is
	 * set, and false while a connection is still coming up, so the ordinary page-load window
	 * before the first handshake reads as pending rather than as a missing broadcast. Waiting
	 * on this is futile by definition; wait for an announcement instead, via the origin's
	 * `announced`.
	 */
	readonly unroutable: Getter<boolean>;

	#dispose: Dispose;
	#closed = false;

	private constructor(
		path: Path.Valid,
		active: Getter<broadcast.Consumer | undefined>,
		unroutable: Getter<boolean>,
		dispose: Dispose,
	) {
		this.path = path;
		this.active = active;
		this.unroutable = unroutable;
		this.#dispose = dispose;
	}

	static {
		makeRequest = (path, active, unroutable, dispose) => new Request(path, active, unroutable, dispose);
	}

	/** Withdraw the request. The path stays routed for any other open request. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#dispose();
	}
}

/**
 * The read side of an origin: resolve broadcasts by path and watch what is available.
 *
 * Obtain one from {@link Producer.consume}. Pass it to a connection's `publish` option to
 * serve the origin's local broadcasts to that peer; read it directly to consume anything
 * the origin routes, locally published or discovered by a session.
 *
 * @public
 */
export class Consumer {
	#state: OriginState;

	private constructor(state: OriginState) {
		this.#state = state;
		// True only when every attached session announces. One session that cannot means the
		// table is an incomplete picture, so a consumer gated on it has to keep its blind
		// fallback: the paths only that session carries never reach the table at all.
		this.#discovery = new Derived([state.sessions], ({ total, discovery }) =>
			total === 0 ? undefined : discovery === total,
		);
	}

	static {
		makeConsumer = (state) => new Consumer(state);
	}

	/** Settles once the origin closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/**
	 * Whether the announcement table sees everything the attached sessions can serve.
	 *
	 * Undefined while no session is attached (nothing is known yet), true when every attached
	 * session announces into the table, and false as soon as one does not, where
	 * {@link announced} cannot be complete and consumers should {@link request} paths instead
	 * of waiting. One blind session among several is still false: the paths only it carries
	 * never reach the table, so a consumer that trusted the gate would never see them.
	 */
	get discovery(): Getter<boolean | undefined> {
		return this.#discovery;
	}

	// Derived per access rather than cached: a lightweight mapped view over the session
	// counts, avoiding a Computed's lifecycle.
	readonly #discovery: Getter<boolean | undefined>;

	/**
	 * Whether the table routes `path`, by a local publish or an announced route
	 * covering it.
	 *
	 * Availability, not a handle: {@link request} is the only way to consume by path. A
	 * request on a routed path resolves to that route and never to a blind answer, which is
	 * why a serving session leaves it alone.
	 *
	 * @internal
	 */
	routes(path: Path.Valid): boolean {
		if (this.#state.local.peek()?.has(path)) return true;
		return this.#state.bestEntry(path) !== undefined;
	}

	/**
	 * Resolve `path`, without waiting for an announcement.
	 *
	 * The one way to consume by path. {@link Request.active} follows whatever the table
	 * routes (a local publish, or any feeding session's announcement, swapping on a
	 * republish); when nothing does, the request stands and whichever attached session
	 * answers first provides a blind subscription instead, re-answered across reconnects.
	 * Close the request when done. On a closed origin it never resolves.
	 *
	 * With several sessions on one origin the first to answer wins, and it may be one that
	 * does not carry the path. Nothing corrects that: a missing broadcast surfaces as a reset
	 * on the first track and deliberately leaves the handle open, since the wire cannot tell
	 * "not here" from "not yet" and a blind handle is expected to survive until a publisher
	 * arrives. It matters only on an origin mixing sessions that announce with sessions that
	 * cannot, where a path only the silent session carries may sit behind another session's
	 * answer. Prefer {@link unroutable} and announcements over blind requests when the origin
	 * feeds from more than one connection.
	 */
	request(path: Path.Valid): Request {
		const requests = this.#state.requests.peek();
		if (!requests) {
			// Closed origin: a request that can never resolve, and says so.
			return makeRequest(path, new Signal<broadcast.Consumer | undefined>(undefined), getter(true), () => {});
		}

		let slot = requests.get(path);
		if (!slot) {
			// Seeded through the constructor, so a path the table already routes resolves on the
			// first read. It must not go through a silent set: that still captures the pre-seed
			// value as the baseline the next change is compared against, and never flushes to
			// clear it, so a seeded route retracting to undefined would look like no change and
			// notify nobody.
			const created: RequestSlot = { count: 0, route: new Signal(this.#state.route(path)) };
			slot = created;
			this.#state.requests.mutate((map) => {
				map?.set(path, created);
			});
		}
		slot.count += 1;

		// Hand out a handle of the request's own rather than the table's. Closing a consumer
		// closes the broadcast once it was the last one, and the table often holds the only
		// other handle, so lending its front out means an ordinary close() by one requester
		// can unpublish the path for everybody else.
		const taken = slot;

		// Memoized on the route's identity: the same front resolving again returns the handle
		// we already made, and only a real swap clones a new one (cloning before closing the
		// old, so a broadcast that both routes share never briefly loses its last handle).
		let released = false;
		let source: broadcast.Consumer | undefined;
		let handle: broadcast.Consumer | undefined;
		const own = (front: broadcast.Consumer | undefined): broadcast.Consumer | undefined => {
			if (released) return undefined;
			if (front !== source) {
				const previous = handle;
				source = front;
				handle = front?.clone();
				previous?.close();
			}
			return handle;
		};

		const route = taken.route;
		const active = new Derived([route], own);

		// Swapping on the read is what keeps a routed path resolving synchronously, but a
		// holder that only ever peeked would then pin a route that has already been retracted
		// until it happened to read again. Following the route as well retires it promptly,
		// and the memo makes the two paths agree: whichever runs first does the swap.
		const unsubscribe = route.subscribe(own);

		// Only meaningful while nothing is routed, so it reads the route rather than `active`:
		// the two cannot disagree, since a routed path always has an answerer-independent
		// answer.
		const unroutable = new Derived([route, this.#state.answerers], (front, answerers) => !front && answerers === 0);

		return makeRequest(path, active, unroutable, () => {
			// Releases this request's handle; the route itself belongs to the table.
			released = true;
			unsubscribe();
			handle?.close();
			handle = undefined;
			source = undefined;

			taken.count -= 1;
			if (taken.count > 0) return;

			// Defer the teardown a microtask: an effect whose rerun was triggered by the
			// answer resolving closes its old request and takes a new one in the same tick,
			// and tearing down in between would drop the answer it is about to read.
			queueMicrotask(() => {
				if (taken.count > 0) return;
				this.#state.requests.mutate((map) => {
					if (map?.get(path) === taken) map.delete(path);
				});
				taken.answer?.close();
				taken.answer = undefined;
				taken.route.set(undefined);
				this.#state.releaseMaterialized(path);
			});
		});
	}

	/**
	 * The announced routes under `prefix`, as a live stream: every currently advertised
	 * prefix arrives first as `active`, then additions and retractions as they happen. A
	 * local broadcast appears only after {@link broadcast.Producer.announce}; a dynamic
	 * or received route announces the prefix it covers. Paths are relative to `prefix`.
	 * The stream ends when the origin closes or the consumer is closed.
	 */
	announced(prefix: Path.Valid = Path.empty()): announce.Consumer {
		const producer = new announce.Producer(prefix);
		void this.#runAnnounced(producer, prefix);
		return producer.consume();
	}

	async #runAnnounced(producer: announce.Producer, prefix: Path.Valid): Promise<void> {
		// Keyed by suffix, valued by identity plus route. Diffing identity rather than
		// mere presence means a republish emits a retraction then a fresh announcement;
		// a re-price of the same identity emits another active (a restart).
		let active = new Map<Path.Valid, Advertised>();

		try {
			for (;;) {
				const local = this.#state.local.peek();
				const advertisedLocal = this.#state.advertisedLocal.peek();
				const routes = this.#state.routes.peek();
				if (local === undefined && advertisedLocal === undefined && routes === undefined) break;

				const next = new Map<Path.Valid, Advertised>();
				// Routes first, so an advertised local at the same path overwrites it: the
				// announcement points at whatever request() would resolve.
				// The most specific route covering `prefix` itself wins the root slot,
				// matching request() resolution.
				let rootLen = -1;
				for (const [path, entries] of routes ?? []) {
					const entry = entries[0];
					if (!entry) continue;
					const snap: Advertised = { identity: entry.identity, route: entry.route.peek() };
					if (Path.hasPrefix(path, prefix)) {
						if (path.length < rootLen) continue;
						rootLen = path.length;
						next.set(Path.empty(), snap);
						continue;
					}
					const suffix = Path.stripPrefix(prefix, path);
					if (suffix !== null) next.set(suffix, snap);
				}
				for (const [path, front] of local ?? []) {
					const route = advertisedLocal?.get(path);
					if (!route) continue;
					const suffix = Path.stripPrefix(prefix, path);
					if (suffix !== null) next.set(suffix, { identity: front, route });
				}

				for (const [path, snap] of active) {
					const cur = next.get(path);
					if (!cur || cur.identity !== snap.identity) producer.append({ prefix: path, active: false });
				}
				for (const [path, snap] of next) {
					const prev = active.get(path);
					if (!prev || prev.identity !== snap.identity || !routesEqual(prev.route, snap.route)) {
						producer.append({ prefix: path, active: true, route: snap.route });
					}
				}
				active = next;

				await Signal.race(this.#state.local, this.#state.advertisedLocal, this.#state.routes, producer.closed);
				if (producer.closed.peek() !== undefined) return;
			}
		} catch {
			// The reader closed between the check and an append; nothing left to do.
		}
		producer.close();
	}

	/**
	 * The local table, borrowed by the wire publishers to answer subscribes.
	 *
	 * Deliberately excludes received routes: a session never re-announces what a peer
	 * told it, so an origin wired to both directions of a connection cannot echo.
	 * Borrowed, not owned: do not close the fronts. Undefined once the origin closes.
	 *
	 * @internal
	 */
	get broadcasts(): Getter<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined> {
		return this.#state.local;
	}

	/**
	 * Originated advertisements a session should forward: exact-path announces plus
	 * originated dynamics. Undefined once the origin closes.
	 *
	 * @internal
	 */
	get advertised(): Getter<ReadonlyMap<Path.Valid, Advertised> | undefined> {
		return this.#state.originated;
	}

	/**
	 * Resolve `path` for serving: a local broadcast, or wait for an originated dynamic
	 * to accept it. Undefined when nothing here can serve the path.
	 *
	 * @internal
	 */
	async demand(path: Path.Valid): Promise<broadcast.Consumer | undefined> {
		const local = this.#state.local.peek()?.get(path);
		if (local) return local;

		const entry = this.#state.bestEntry(path);
		if (!entry?.originated || !entry.server) return undefined;

		const server = entry.server;
		const live = server.served.get(path);
		if (live && live.closed.peek() === undefined) return live;

		server.enqueue(path);
		for (;;) {
			const served = server.served.get(path);
			if (served && served.closed.peek() === undefined) return served;
			if (!server.pending.has(path)) return undefined;
			const closed = server.closed.peek();
			if (closed !== undefined) return undefined;
			await Signal.race(server.settled, server.closed);
		}
	}
}

/**
 * A served route from {@link Producer.dynamic}: advertises a prefix and answers the
 * requests beneath it.
 *
 * Drop it (or {@link close}) to retract the route and reject anything still waiting
 * with {@link StreamCode.NoCapacity}. {@link update} re-prices it in place.
 *
 * @public
 */
export class Dynamic {
	/** The pattern this handle advertises. */
	readonly pattern: Path.Pattern;

	#prefix: Path.Valid;
	#entry: RouteEntry;
	#state: OriginState;
	#retract: Dispose;
	#closed = false;

	private constructor(
		pattern: Path.Pattern,
		prefix: Path.Valid,
		entry: RouteEntry,
		state: OriginState,
		retract: Dispose,
	) {
		this.pattern = pattern;
		this.#prefix = prefix;
		this.#entry = entry;
		this.#state = state;
		this.#retract = retract;
	}

	static {
		makeDynamic = (pattern, prefix, entry, state, retract) => new Dynamic(pattern, prefix, entry, state, retract);
	}

	/** Re-price the route in place. The prefix is fixed at announce time. */
	update(route: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint }): void {
		if (this.#closed) throw new Error("dynamic is closed");
		this.#entry.route.set(normalizeRoute(route));
		this.#state.rebuildOriginated();
		this.#state.refreshPrefix(this.#prefix);
		this.#state.routes.mutate(() => {});
	}

	/** Retract the route and reject anything still waiting. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#retract();
	}

	/** Requests under this prefix, as they arrive, each to {@link BroadcastRequest.accept} or reject. */
	async *requested(): AsyncIterableIterator<BroadcastRequest> {
		const server = this.#entry.server;
		if (!server) return;
		for (;;) {
			const next = server.queue.peek()[0];
			if (next) {
				server.queue.mutate((queue) => {
					queue.shift();
				});
				yield next;
				continue;
			}
			if (server.closed.peek() !== undefined) return;
			await Signal.race(server.queue, server.closed);
		}
	}
}

/**
 * A pending request for a broadcast to be served on demand.
 *
 * Yielded by {@link Dynamic.requested}. {@link accept} resolves it with a live
 * broadcast; {@link reject} resolves it with an error. Dropping it without either
 * rejects it.
 *
 * @public
 */
export class BroadcastRequest {
	/** The path that was requested. */
	readonly path: Path.Valid;

	#server: ServeState;
	#done = false;

	private constructor(path: Path.Valid, server: ServeState) {
		this.path = path;
		this.#server = server;
	}

	static {
		makeBroadcastRequest = (path, server) => new BroadcastRequest(path, server);
		finishBroadcastRequest = (request, err) => {
			request.#done = true;
			void err;
		};
	}

	/**
	 * Accept the request, resolving every awaiting requester with `broadcast`.
	 *
	 * The caller keeps producing into `broadcast`; repeat requests for the path share
	 * it for as long as it stays live.
	 */
	accept(source: broadcast.Producer | broadcast.Consumer): void {
		if (this.#done) return;
		this.#done = true;
		const front = source instanceof broadcast.Producer ? source.consume() : source;
		this.#server.accept(this, front);
	}

	/** Reject the request, resolving every awaiting requester with `err`. */
	reject(err: Error): void {
		if (this.#done) return;
		this.#done = true;
		this.#server.reject(this, err);
	}
}

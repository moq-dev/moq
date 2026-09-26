/**
 * A broadcast routing table, independent of any connection.
 *
 * Publish broadcasts into an origin and hand the origin to one or more connections to
 * serve them; the broadcasts outlive any single session. Hand the same (or another)
 * origin to a connection's `consume` option and the peer's announced routes appear
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
import { isAnonymous, Route, routesEqual } from "./hop.ts";
import { hiddenBelow, hooks, scopeCaptures, scopeHead, scopeOverlaps } from "./internal.ts";
import * as Path from "./path.ts";
import { type Advertised, registerWire, wireOf } from "./wire.ts";

export type { Cost, Hop, Route } from "./hop.ts";
export { isAnonymous } from "./hop.ts";

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
 * `handles` holds the `closed` of each open {@link Requesting} on the path.
 *
 * A refusal is terminal. With nothing serving, the slot ends: every handle closes with the
 * handler's error and the slot leaves the table, so the next request asks afresh. While
 * another source still serves (a better route was asked and said no), the refuser joins
 * `refused` and is skipped for as long as it stands (a reconnect is a fresh entry), so the
 * current source carries on. A refusal never falls through to a broader prefix or another
 * advertiser.
 *
 * @internal
 */
export interface RequestSlot {
	blind: number;
	answer?: broadcast.Consumer;
	readonly handles: Set<Once<Error | null>>;
	readonly refused: Set<RouteEntry>;
	readonly route: Signal<broadcast.Consumer | undefined>;
}

/**
 * One advertised prefix: hops and cost, plus an optional server that answers
 * requests beneath it.
 *
 * The preferred entry per prefix is the one requests resolve through. An originated
 * entry is forwarded by sessions; a received one is not, so a shared origin
 * cannot echo a peer's announcements back to it.
 *
 * @internal
 */
export interface RouteEntry {
	readonly identity: object;
	readonly route: Signal<Route>;
	readonly originated: boolean;
	readonly server?: ServeState;
}

/** Orders two routes by preference: identified before anonymous, then lower warm cost, then lower cold cost. */
function compareRoutes(a: Route, b: Route): number {
	const anonymous = Number(isAnonymous(a)) - Number(isAnonymous(b));
	if (anonymous !== 0) return anonymous;
	if (a.cost.warm !== b.cost.warm) return a.cost.warm < b.cost.warm ? -1 : 1;
	if (a.cost.cold !== b.cost.cold) return a.cost.cold < b.cost.cold ? -1 : 1;
	return 0;
}

/** The preferred of `entries` (newest first) not skipped: the best route, then fewest hops, then newest. */
function preferredEntry(entries: readonly RouteEntry[], skip?: (entry: RouteEntry) => boolean): RouteEntry | undefined {
	let best: RouteEntry | undefined;
	for (const entry of entries) {
		if (skip?.(entry)) continue;
		if (!best) {
			best = entry;
			continue;
		}
		const a = entry.route.peek();
		const b = best.route.peek();
		const order = compareRoutes(a, b) || a.hops.length - b.hops.length;
		if (order < 0) best = entry;
	}
	return best;
}

/** Whether a session received `entry`, so it is never forwarded to a peer. */
function received(entry: RouteEntry): boolean {
	return !entry.originated;
}

function noCapacity(): StreamError {
	return new StreamError(StreamCode.NoCapacity, { message: "no capacity" });
}

/** A served route from {@link Producer.dynamic}: the queue a handler drains. */
class ServeState {
	queue = new Signal<Request[]>([]);
	pending = new Map<Path.Valid, Request>();
	served = new Map<Path.Valid, broadcast.Consumer>();
	rejected = new Map<Path.Valid, Error>();
	// demand() is the only reader of `rejected`. A Consumer.request refusal never
	// re-enqueues, so storing the error without a waiter would pin every unique
	// path until the route dies.
	demanding = new Map<Path.Valid, number>();
	closed = new Once<Error | null>();
	settled = new Signal(0);
	onChange: (path: Path.Valid) => void = () => {};
	onReject: (path: Path.Valid, err: Error) => void = () => {};

	enqueue(path: Path.Valid): void {
		if (this.closed.peek() !== undefined) return;
		this.rejected.delete(path);
		if (this.pending.has(path)) return;
		const live = this.served.get(path);
		if (live && live.closed.peek() === undefined) return;
		const request = makeRequest(path, this);
		this.pending.set(path, request);
		this.queue.mutate((queue) => {
			queue.push(request);
		});
	}

	accept(request: Request, front: broadcast.Consumer): void {
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
			if (this.served.get(request.path) !== front) return;
			this.served.delete(request.path);
			this.onChange(request.path);
		});
		this.onChange(request.path);
		this.settled.update((n) => n + 1);
	}

	reject(request: Request, err: Error): void {
		if (this.pending.get(request.path) !== request) return;
		this.pending.delete(request.path);
		if (this.demanding.has(request.path)) this.rejected.set(request.path, err);
		this.onReject(request.path, err);
		this.settled.update((n) => n + 1);
	}

	close(abort?: Error): void {
		if (this.closed.peek() !== undefined) return;
		const err = abort ?? noCapacity();
		this.closed.set(err);
		const queued = [...this.pending.values()];
		this.pending.clear();
		this.queue.mutate((queue) => {
			queue.length = 0;
		});
		for (const request of queued) {
			finishRequest(request, err);
		}
		for (const [path, front] of this.served) {
			front.close(abort);
			this.onChange(path);
		}
		this.served.clear();
		this.rejected.clear();
		this.demanding.clear();
		this.settled.update((n) => n + 1);
	}
}

interface Presented extends Advertised {
	readonly captures: Path.Pattern[] | undefined;
}

/** A table mutation invalidates the shared route snapshot before its async notification. */
class VersionedSignal<T> extends Signal<T> {
	version = 0;

	override set(value: T, notify?: boolean): void {
		this.version++;
		super.set(value, notify);
	}
}

/** Reactive backing state shared by origin producers and consumers. */
class OriginState {
	// Both tables decouple the application producing into the origin from the
	// connections serving or feeding it. Undefined once the origin closes, so late
	// writes fail loudly.
	//
	// Created is what this endpoint publishes, keyed by exact path, announced or not.
	// Local is the announced subset, with its route in advertisedLocal: a broadcast
	// exists for nobody, here or at a peer, until it announces. Routes is the
	// advertisement table: prefixes a dynamic handle or a received session covers,
	// newest first. Local and routes stay separate so a session can never announce a
	// received entry back to a peer, which is what makes an origin shared by both
	// directions echo-free.
	created: Map<Path.Valid, broadcast.Consumer> | undefined = new Map();
	local = new VersionedSignal<Map<Path.Valid, broadcast.Consumer> | undefined>(new Map());
	advertisedLocal = new VersionedSignal<Map<Path.Valid, Route> | undefined>(new Map());
	routes = new VersionedSignal<Map<Path.Valid, RouteEntry[]> | undefined>(new Map());

	#snapshotVersion = "";
	#snapshot = {
		remote: new Map<Path.Valid, Advertised>(),
		local: new Map<Path.Valid, Advertised>(),
		routes: new Map<Path.Valid, Route>(),
		visible: new Map<Path.Valid, Route>(),
	};

	/** The full route table is built once per mutation, regardless of observer count. */
	available = new Derived([this.local, this.advertisedLocal, this.routes], () => this.snapshot().routes);
	/** {@link available} without hidden routes, for unscoped readers that did not opt in. */
	visible = new Derived([this.local, this.advertisedLocal, this.routes], () => this.snapshot().visible);

	snapshot(): {
		remote: ReadonlyMap<Path.Valid, Advertised>;
		local: ReadonlyMap<Path.Valid, Advertised>;
		routes: ReadonlyMap<Path.Valid, Route>;
		visible: ReadonlyMap<Path.Valid, Route>;
	} {
		const version = `${this.local.version}/${this.advertisedLocal.version}/${this.routes.version}`;
		if (version === this.#snapshotVersion) return this.#snapshot;
		const remote = new Map<Path.Valid, Advertised>();
		const local = new Map<Path.Valid, Advertised>();
		const available = new Map<Path.Valid, Route>();
		for (const [path, routes] of this.routes.peek() ?? []) {
			const entry = preferredEntry(routes);
			if (!entry) continue;
			const value = { identity: entry.identity, route: entry.route.peek() };
			remote.set(path, value);
			available.set(path, value.route);
		}
		for (const [path, front] of this.local.peek() ?? []) {
			const routes = this.routes.peek()?.get(path);
			if (!this.localWins(path, routes && preferredEntry(routes))) continue;
			const value = { identity: front, route: this.advertisedLocal.peek()?.get(path) ?? Route.default };
			local.set(path, value);
			available.set(path, value.route);
		}
		const visible = new Map<Path.Valid, Route>();
		for (const [path, route] of available) {
			if (!hiddenBelow(Path.empty(), path)) visible.set(path, route);
		}
		this.#snapshot = { remote, local, routes: available, visible };
		this.#snapshotVersion = version;
		return this.#snapshot;
	}

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
		slot.route.set(this.route(path, slot));
	}

	/**
	 * `entry` refused `path` with `err`. A request still serving another source skips the
	 * refuser; one with nothing serving ends with `err`.
	 */
	refuse(path: Path.Valid, entry: RouteEntry, err: Error): void {
		const slot = this.requests.peek()?.get(path);
		if (!slot) return;
		// Only the route the request is waiting on speaks for it; a superseded one's answer is moot.
		if (this.bestEntry(path, (candidate) => slot.refused.has(candidate)) !== entry) return;

		const serving = slot.route.peek();
		if (serving && serving.closed.peek() === undefined) {
			slot.refused.add(entry);
			slot.route.set(this.route(path, slot));
			return;
		}

		this.requests.mutate((map) => {
			if (map?.get(path) === slot) map.delete(path);
		});
		slot.answer?.close();
		slot.answer = undefined;
		slot.route.set(undefined);
		this.releaseMaterialized(path);
		for (const closed of slot.handles) closed.set(err);
		slot.handles.clear();
	}

	/**
	 * Recompute every open request covered by `prefix`, after a route was inserted or
	 * removed there: a route covers many paths, so a single-path refresh is not enough.
	 * Every materialized broadcast belongs to an open request, so rerouting them also
	 * releases a retracted route's session subscription even when nothing reads it again.
	 */
	refreshPrefix(prefix: Path.Valid): void {
		for (const [path, slot] of this.requests.peek() ?? []) {
			if (Path.hasPrefix(prefix, path)) slot.route.set(this.route(path, slot));
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
		for (const [prefix, entries] of routes ?? []) {
			const mine = preferredEntry(entries, received);
			if (mine) next.set(prefix, { identity: mine.identity, route: mine.route.peek() });
		}
		// A local broadcast and an originated dynamic at one path compete on cost, as they do for requests.
		for (const [path, route] of advertised ?? []) {
			const front = local?.get(path);
			const entries = routes?.get(path);
			if (front && this.localWins(path, entries && preferredEntry(entries, received))) {
				next.set(path, { identity: front, route });
			}
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

	/** The preferred entry on the most specific route covering `path`, ignoring skipped entries, if any. */
	bestEntry(path: Path.Valid, skip?: (entry: RouteEntry) => boolean): RouteEntry | undefined {
		let bestPrefix: Path.Valid | undefined;
		let best: RouteEntry | undefined;
		for (const [prefix, entries] of this.routes.peek() ?? []) {
			if (!Path.hasPrefix(prefix, path)) continue;
			const entry = preferredEntry(entries, skip);
			if (!entry) continue;
			if (bestPrefix === undefined || prefix.length > bestPrefix.length) {
				bestPrefix = prefix;
				best = entry;
			}
		}
		return best;
	}

	/**
	 * Whether the announced local broadcast at `path` wins over `entry`, the best route a
	 * session or dynamic handle announced there. Cost decides, as for any two routes: an
	 * identified route strictly cheaper than the local one wins, and the local broadcast
	 * wins a tie. A route at a shorter prefix never competes, since the most specific
	 * prefix wins outright. False when nothing is announced locally at `path`.
	 */
	localWins(path: Path.Valid, entry: RouteEntry | undefined): boolean {
		const local = this.advertisedLocal.peek()?.get(path);
		if (!local || !this.local.peek()?.has(path)) return false;
		if (!entry || !this.routes.peek()?.get(path)?.includes(entry)) return true;
		return compareRoutes(local, entry.route.peek()) <= 0;
	}

	/**
	 * What `path` resolves to: an announced local publish, a broadcast materialized from
	 * the best covering route, or the blind answer.
	 *
	 * Materialization is lazy and cached per path: the first request under a route opens
	 * the providing session's subscription and repeats share it. A better route is made
	 * before the old one breaks: the current front keeps serving until the new route
	 * answers (then swaps) or refuses (then is skipped). A retracted route swaps at once.
	 */
	route(path: Path.Valid, slot: Pick<RequestSlot, "answer" | "refused">): broadcast.Consumer | undefined {
		const entry = this.bestEntry(path, (candidate) => slot.refused.has(candidate));
		const local = this.local.peek()?.get(path);
		if (local && this.localWins(path, entry)) {
			// Nothing reads a remote front the local broadcast replaced, so close its session subscription.
			this.releaseMaterialized(path);
			return local;
		}

		let cached = this.materialized.get(path);
		if (cached && cached.front.closed.peek() !== undefined) {
			this.materialized.delete(path);
			cached = undefined;
		}
		if (cached && cached.entry === entry) return cached.front;
		if (!entry?.server) {
			this.releaseMaterialized(path);
			return slot.answer;
		}

		const served = entry.server.served.get(path);
		if (served && served.closed.peek() === undefined) {
			cached?.front.close();
			this.materialized.set(path, { entry, front: served });
			return served;
		}

		entry.server.enqueue(path);
		return cached?.front;
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

	/** Create an unannounced broadcast at `path`; see {@link Producer.createBroadcast}. */
	createBroadcast(path: Path.Valid): broadcast.Producer;

	/** Resolve `path`, optionally waiting for an announcement; see {@link Consumer.request}. */
	request(path: Path.Valid, options?: RequestOptions): Requesting;

	/** The available announcements under `scope`, as a live map; see {@link Consumer.broadcasts}. */
	broadcasts(scope?: Path.Pattern, options?: announce.Options): Getter<ReadonlyMap<Path.Valid, Route>>;

	/** The available broadcasts under `scope`, as a live stream; see {@link Consumer.announced}. */
	announced(scope?: Path.Pattern, options?: announce.Options): announce.Consumer;

	/** Advertise a prefix and serve requests under it; see {@link Producer.dynamic}. */
	dynamic(prefix: Path.Valid, route?: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint }): Dynamic;
}

/** Options for resolving a broadcast path. */
export interface RequestOptions {
	/** Wait for a routed announcement when discovery is supported; otherwise subscribe blindly. */
	announced?: boolean;
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
 * refused, and nobody can see or reach a broadcast until it announces.
 *
 * @public
 */
export class Producer implements Table {
	#state = new OriginState();

	// The reader backing the passthroughs, so holding a Producer never requires the
	// consume().x() stutter for everyday reads. One instance, so `discovery` keeps its
	// identity across reads.
	#reader = makeConsumer(this.#state);

	constructor() {
		const thisProducer = this;
		registerWire(this, {
			receive: (prefix, route) => this.#receive(prefix, route),
			attach: (discovery) => this.#attach(discovery),
			expect: () => this.#expect(),
			get requests() {
				return thisProducer.#state.requests;
			},
			changed: () => this.#changed(),
			answer: (path, front) => this.#answer(path, front),
			routes: (path) => wireOf(this.#reader).routes(path),
		});
	}

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
	 * The broadcast exists for nobody until {@link broadcast.Producer.announce}: announce
	 * streams skip it and requests for its path find nothing, on this origin exactly as at
	 * a peer. Announce once its tracks exist; {@link broadcast.Producer.unannounce}
	 * withdraws it from everyone again.
	 *
	 * Close the producer to drop it. Creating a path again supersedes the previous
	 * broadcast: the origin drops its handle on the old one, which closes it unless the
	 * application still holds a consumer clone. An announced local broadcast competes with
	 * a remote route at the same path on cost, winning ties.
	 */
	createBroadcast(path: Path.Valid): broadcast.Producer {
		const created = this.#state.created;
		if (!created) throw new Error("origin is closed");

		const producer = new broadcast.Producer();
		const front = producer.consume();

		hooks.attachAnnouncer(producer, {
			announce: (route) => this.#advertiseExact(path, front, route),
			unannounce: () => this.#retractExact(path, front),
		});

		const previous = created.get(path);
		created.set(path, front);
		if (previous) {
			this.#retractExact(path, previous);
			previous.close();
		}

		// Drop it when the broadcast closes, unless a recreate already replaced it: a
		// stale broadcast closing must not unpublish the live one.
		void front.closed.then(() => {
			this.#retractExact(path, front);
			if (this.#state.created?.get(path) === front) this.#state.created.delete(path);
		});

		return producer;
	}

	#advertiseExact(path: Path.Valid, front: broadcast.Consumer, route: Route): void {
		if (!this.#state.local.peek()) throw new Error("origin is closed");
		if (this.#state.created?.get(path) !== front) throw new Error("broadcast is closed");
		// Both maps move together, so every reader sees the broadcast and its route at once.
		this.#state.local.mutate((broadcasts) => {
			broadcasts?.set(path, front);
		});
		this.#state.advertisedLocal.mutate((advertised) => {
			advertised?.set(path, route);
		});
		this.#state.rebuildOriginated();
		this.#state.refresh(path);
	}

	#retractExact(path: Path.Valid, front: broadcast.Consumer): void {
		if (this.#state.local.peek()?.get(path) !== front) return;
		this.#state.local.mutate((broadcasts) => {
			broadcasts?.delete(path);
		});
		this.#state.advertisedLocal.mutate((advertised) => {
			advertised?.delete(path);
		});
		this.#state.rebuildOriginated();
		this.#state.refresh(path);
	}

	/**
	 * Advertise `prefix` and serve the requests beneath it.
	 *
	 * A route is always a prefix: it claims `prefix` and every path beneath it (the
	 * empty prefix claims every path). A service that only serves some of them
	 * advertises the covering prefix and rejects the rest as they are requested;
	 * consumers narrow with a {@link Path.Pattern} locally. The advertisement is
	 * visible to {@link Consumer.announced} and forwarded by sessions for as long as
	 * the returned {@link Dynamic} lives. A consumer resolving a path under it that
	 * no announced local broadcast wins is handed to the handle as a {@link Request}.
	 */
	dynamic(
		prefix: Path.Valid,
		route: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint } = Route.default,
	): Dynamic {
		return this.#insertRoute(prefix, Route.normalize(route), true);
	}

	/**
	 * Land a route a peer announced, served through the returned handle. Same as
	 * {@link dynamic} but not originated, so a session never announces it back.
	 *
	 * @internal
	 */
	#receive(
		prefix: Path.Valid,
		route: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint } = Route.default,
	): Dynamic {
		return this.#insertRoute(prefix, Route.normalize(route), false);
	}

	#insertRoute(prefix: Path.Valid, route: Route, originated: boolean): Dynamic {
		const server = new ServeState();
		server.onChange = (path) => this.#state.refresh(path);
		const entry: RouteEntry = {
			identity: {},
			route: new Signal(route),
			originated,
			server,
		};
		server.onReject = (path, err) => this.#state.refuse(path, entry, err);

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
			return makeDynamic(prefix, entry, this.#state, () => {});
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
			// A retracted entry can never be picked again, so the refusals pinned to it are dead weight.
			for (const slot of this.#state.requests.peek()?.values() ?? []) slot.refused.delete(entry);
			server.close();
			this.#state.rebuildOriginated();
			this.#state.refreshPrefix(prefix);
		};

		return makeDynamic(prefix, entry, this.#state, retract);
	}

	/**
	 * Register an attached session, counting it toward the `discovery` state. Returns the
	 * detach; call it exactly once when the session dies.
	 *
	 * @internal
	 */
	#attach(discovery: boolean): Dispose {
		this.#sessions(1, discovery);
		const release = this.#expect();
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
	#expect(): Dispose {
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
	 * Resolves once anything a serving session scans changes: the open requests, or either
	 * side of the routing table.
	 *
	 * @internal
	 */
	#changed(): GetPromise<unknown> {
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
	#answer(path: Path.Valid, front: broadcast.Consumer): Dispose | undefined {
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

	/** Resolve `path`, optionally waiting for an announcement; see {@link Consumer.request}. */
	request(path: Path.Valid, options?: RequestOptions): Requesting {
		return this.#reader.request(path, options);
	}

	/** The available announcements under `scope`, as a live map; see {@link Consumer.broadcasts}. */
	broadcasts(scope?: Path.Pattern, options?: announce.Options): Getter<ReadonlyMap<Path.Valid, Route>> {
		return this.#reader.broadcasts(scope, options);
	}

	/** The available broadcasts under `scope`, as a live stream; see {@link Consumer.announced}. */
	announced(scope?: Path.Pattern, options?: announce.Options): announce.Consumer {
		return this.#reader.announced(scope, options);
	}

	/** Close the origin, every broadcast it still routes, and its announcement streams. Idempotent. */
	close(abort?: Error) {
		if (this.#state.closed.peek() !== undefined) return;
		this.#state.closed.set(abort ?? null);
		for (const front of this.#state.created?.values() ?? []) {
			front.close(abort);
		}
		this.#state.created = undefined;
		this.#state.local.update(() => undefined);
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

// Same for Requesting: a public constructor would let a caller forge a handle that no origin
// ever registered, whose lifecycle guarantees are then false. `@internal` alone would not
// stop it, since the declaration emit keeps the constructor.
let makeRequesting: (
	path: Path.Valid,
	active: Getter<broadcast.Consumer | undefined>,
	unroutable: Getter<boolean>,
	closed: Once<Error | null>,
	dispose: Dispose,
) => Requesting;

let makeDynamic: (prefix: Path.Valid, entry: RouteEntry, state: OriginState, retract: Dispose) => Dynamic;

let makeRequest: (path: Path.Valid, server: ServeState) => Request;
let finishRequest: (request: Request, err: Error) => void;

/**
 * An open request for a path nothing announced; see {@link Consumer.request}.
 *
 * @public
 */
export class Requesting {
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
	 * `announced`. True once the request is refused.
	 */
	readonly unroutable: Getter<boolean>;

	/**
	 * Settles with the error a route's handler refused the path with, or `null` once you
	 * {@link close} the request. A refusal is final: no other route is asked, and a fresh
	 * request is needed to try again.
	 */
	readonly closed: GetPromise<Error | null>;

	#dispose: Dispose;
	#disposed = false;

	private constructor(
		path: Path.Valid,
		active: Getter<broadcast.Consumer | undefined>,
		unroutable: Getter<boolean>,
		closed: GetPromise<Error | null>,
		dispose: Dispose,
	) {
		this.path = path;
		this.active = active;
		this.unroutable = unroutable;
		this.closed = closed;
		this.#dispose = dispose;
	}

	static {
		makeRequesting = (path, active, unroutable, closed, dispose) =>
			new Requesting(path, active, unroutable, closed, dispose);
	}

	/** Withdraw the request. The path stays routed for any other open request. Idempotent. */
	close(): void {
		if (this.#disposed) return;
		this.#disposed = true;
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
		registerWire(this, {
			routes: (path) => this.#routes(path),
			get broadcasts() {
				return state.local;
			},
			get advertised() {
				return state.originated;
			},
			local: (path) => this.#local(path),
			demand: (path) => this.#demand(path),
		});
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
	 * Whether the table routes `path`, by an announced local publish or an announced
	 * route covering it.
	 *
	 * Availability, not a handle: {@link request} is the only way to consume by path. A
	 * request on a routed path resolves to that route and never to a blind answer, which is
	 * why a serving session leaves it alone.
	 *
	 * @internal
	 */
	#routes(path: Path.Valid): boolean {
		if (this.#state.local.peek()?.has(path)) return true;
		return this.#state.bestEntry(path) !== undefined;
	}

	/**
	 * Resolve `path`, optionally waiting for an announcement.
	 *
	 * The one way to consume by path. {@link Requesting.active} follows whatever the table
	 * routes (an announced local publish, or any feeding session's announcement, swapping
	 * on a republish or a retraction); when nothing does, the request stands and whichever
	 * attached session answers first provides a blind subscription instead, re-answered
	 * across reconnects.
	 * With `announced: true`, an unrouted request waits while discovery is supported and
	 * falls back to that blind behavior only when discovery is unavailable. Close the request
	 * when done. On a closed origin it never resolves.
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
	request(path: Path.Valid, options: RequestOptions = {}): Requesting {
		const requests = this.#state.requests.peek();
		if (!requests) {
			// Closed origin: a request that can never resolve, and says so.
			const closed = new Once<Error | null>();
			return makeRequesting(
				path,
				new Signal<broadcast.Consumer | undefined>(undefined),
				getter(true),
				closed,
				() => closed.set(null),
			);
		}

		let slot = requests.get(path);
		if (!slot) {
			// Seeded through the constructor, so a path the table already routes resolves on the
			// first read. It must not go through a silent set: that still captures the pre-seed
			// value as the baseline the next change is compared against, and never flushes to
			// clear it, so a seeded route retracting to undefined would look like no change and
			// notify nobody.
			const refused = new Set<RouteEntry>();
			const created: RequestSlot = {
				blind: 0,
				handles: new Set(),
				refused,
				route: new Signal(this.#state.route(path, { refused })),
			};
			slot = created;
			this.#state.requests.mutate((map) => {
				map?.set(path, created);
			});
		}
		const closed = new Once<Error | null>();
		slot.handles.add(closed);
		let blind = !options.announced || this.#discovery.peek() === false;
		if (blind) slot.blind += 1;
		this.#state.requests.mutate(() => {});

		// An announcement-gated request falls back to a blind subscription only while at
		// least one attached session cannot announce. It returns to the gate if discovery
		// becomes complete again, and remains gated with no session attached.
		const unsubscribeDiscovery = options.announced
			? this.#discovery.subscribe((discovery) => {
					const next = discovery === false;
					if (next === blind) return;
					blind = next;
					slot.blind += next ? 1 : -1;
					this.#state.requests.mutate(() => {});
				})
			: () => {};

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
		const unroutable = new Derived(
			[route, this.#state.answerers, closed],
			(front, answerers, ended) => ended !== undefined || (!front && answerers === 0),
		);

		return makeRequesting(path, active, unroutable, closed, () => {
			// Releases this request's handle; the route itself belongs to the table.
			released = true;
			unsubscribeDiscovery();
			unsubscribe();
			handle?.close();
			handle = undefined;
			source = undefined;

			taken.handles.delete(closed);
			if (closed.peek() === undefined) closed.set(null);
			if (blind) taken.blind -= 1;
			this.#state.requests.mutate(() => {});
			if (taken.handles.size > 0) return;

			// Defer the teardown a microtask: an effect whose rerun was triggered by the
			// answer resolving closes its old request and takes a new one in the same tick,
			// and tearing down in between would drop the answer it is about to read.
			queueMicrotask(() => {
				if (taken.handles.size > 0) return;
				// A refused slot already tore itself down, and the path may hold a newer one.
				if (this.#state.requests.peek()?.get(path) !== taken) return;
				this.#state.requests.mutate((map) => {
					map?.delete(path);
				});
				taken.answer?.close();
				taken.answer = undefined;
				taken.route.set(undefined);
				this.#state.releaseMaterialized(path);
			});
		});
	}

	/**
	 * The announced routes matching `scope`, as a live map from covered prefix to route.
	 * Local broadcasts appear once announced; received and dynamic routes retain their
	 * advertised prefixes. Reads are synchronous, and the getter needs no teardown.
	 * Hidden routes are left out unless `options.hidden` opts in (see {@link announce.Options}).
	 * Unscoped readers share one snapshot; each distinct scope filters the table on changes.
	 */
	broadcasts(scope?: Path.Pattern, options?: announce.Options): Getter<ReadonlyMap<Path.Valid, Route>> {
		const hidden = options?.hidden ?? false;
		if (!scope) return hidden ? this.#state.available : this.#state.visible;
		return new Derived([this.#state.available], () => {
			const routes = new Map<Path.Valid, Route>();
			for (const [path, entry] of this.#listed(scope, hidden)) routes.set(path, entry.route);
			return routes;
		});
	}

	/**
	 * The announced routes matching `scope`, as a live stream: every currently advertised
	 * route arrives first as active, then additions and retractions as they happen.
	 * Any pattern is accepted. A local broadcast appears once it announces, exactly as a
	 * peer sees it. A dynamic or received route announces the prefix it covers when its
	 * subtree overlaps the scope. The stream ends when the origin closes or the consumer is
	 * closed. Hidden routes are left out unless `options.hidden` opts in (see {@link announce.Options}).
	 */
	announced(scope: Path.Pattern = Path.Pattern.all(), options?: announce.Options): announce.Consumer {
		const producer = new announce.Producer();
		void this.#runAnnounced(producer, scope, options?.hidden ?? false);
		return producer.consume();
	}

	/** One snapshot shared by map readers and announcement-stream diffing. */
	#listed(scope: Path.Pattern, hidden: boolean): Map<Path.Valid, Presented> {
		const next = new Map<Path.Valid, Presented>();
		const { remote, local } = this.#state.snapshot();
		const head = scopeHead(scope);
		for (const [path, entry] of remote) {
			if (!scopeOverlaps(scope, path)) continue;
			if (!hidden && hiddenBelow(head, path)) continue;
			next.set(path, { identity: entry.identity, route: entry.route, captures: scopeCaptures(scope, path) });
		}
		for (const [path, entry] of local) {
			if (!scope.matches(path)) continue;
			if (!hidden && hiddenBelow(head, path)) continue;
			next.set(path, { identity: entry.identity, route: entry.route, captures: scopeCaptures(scope, path) });
		}
		return next;
	}

	async #runAnnounced(producer: announce.Producer, scope: Path.Pattern, hidden: boolean): Promise<void> {
		// Keyed by the presented path (from the origin, not the scope), valued by identity
		// plus route. Diffing identity rather than mere presence means a republish emits a
		// retraction then a fresh announcement; a re-price of the same identity emits an
		// update.
		let active = new Map<Path.Valid, Presented>();

		try {
			for (;;) {
				const local = this.#state.local.peek();
				const advertisedLocal = this.#state.advertisedLocal.peek();
				const routes = this.#state.routes.peek();
				if (local === undefined && advertisedLocal === undefined && routes === undefined) break;

				const next = this.#listed(scope, hidden);

				for (const [path, snap] of active) {
					const cur = next.get(path);
					if (!cur || cur.identity !== snap.identity)
						producer.append({
							prefix: path,
							captures: snap.captures,
							kind: "retracted",
							route: snap.route,
						});
				}
				for (const [path, snap] of next) {
					const prev = active.get(path);
					if (!prev || prev.identity !== snap.identity) {
						producer.append({
							prefix: path,
							captures: snap.captures,
							kind: "announced",
							route: snap.route,
						});
					} else if (!routesEqual(prev.route, snap.route)) {
						producer.append({ prefix: path, captures: snap.captures, kind: "updated", route: snap.route });
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
	/**
	 * Originated advertisements a session should forward: exact-path announces plus
	 * originated dynamics. Undefined once the origin closes.
	 *
	 * @internal
	 */
	/**
	 * The announced local broadcast at `path`, when it beats the originated routes there.
	 * Resolves through what rebuildOriginated advertised: a peer never sees received routes.
	 */
	#local(path: Path.Valid): broadcast.Consumer | undefined {
		const local = this.#state.local.peek()?.get(path);
		if (local && this.#state.localWins(path, this.#state.bestEntry(path, received))) return local;
		return undefined;
	}

	/**
	 * Resolve `path` for serving: an announced local broadcast, or wait for an originated
	 * dynamic to accept it. Undefined when nothing here can serve the path.
	 *
	 * @internal
	 */
	async #demand(path: Path.Valid): Promise<broadcast.Consumer | undefined> {
		const local = this.#local(path);
		if (local) return local;
		const entry = this.#state.bestEntry(path, received);
		if (!entry?.server) return undefined;

		const server = entry.server;
		const live = server.served.get(path);
		if (live && live.closed.peek() === undefined) return live;

		server.enqueue(path);
		server.demanding.set(path, (server.demanding.get(path) ?? 0) + 1);
		try {
			for (;;) {
				const served = server.served.get(path);
				if (served && served.closed.peek() === undefined) return served;
				const rejected = server.rejected.get(path);
				if (rejected) {
					server.rejected.delete(path);
					throw rejected;
				}
				const closed = server.closed.peek();
				if (closed !== undefined) {
					if (closed) throw closed;
					return undefined;
				}
				if (!server.pending.has(path)) return undefined;
				await Signal.race(server.settled, server.closed);
			}
		} finally {
			const n = (server.demanding.get(path) ?? 1) - 1;
			if (n <= 0) server.demanding.delete(path);
			else server.demanding.set(path, n);
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
	/** The prefix this handle advertises. */
	readonly prefix: Path.Valid;

	#entry: RouteEntry;
	#state: OriginState;
	#retract: Dispose;
	#closed = false;

	private constructor(prefix: Path.Valid, entry: RouteEntry, state: OriginState, retract: Dispose) {
		this.prefix = prefix;
		this.#entry = entry;
		this.#state = state;
		this.#retract = retract;
	}

	static {
		makeDynamic = (prefix, entry, state, retract) => new Dynamic(prefix, entry, state, retract);
	}

	/** Re-price the route in place. The prefix is fixed at announce time. */
	update(route: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint }): void {
		if (this.#closed) throw new Error("dynamic is closed");
		this.#entry.route.set(Route.normalize(route));
		this.#state.rebuildOriginated();
		this.#state.refreshPrefix(this.prefix);
		this.#state.routes.mutate(() => {});
	}

	/** Retract the route and reject anything still waiting. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#retract();
	}

	/** Requests under this prefix, as they arrive, each to {@link Request.accept} or reject. */
	async *requested(): AsyncIterableIterator<Request> {
		const server = this.#entry.server;
		if (!server) return;
		let current: Request | undefined;
		const drop = () => {
			current?.reject(noCapacity());
			current = undefined;
		};
		try {
			for (;;) {
				const next = server.queue.peek()[0];
				if (next) {
					drop();
					server.queue.mutate((queue) => {
						queue.shift();
					});
					current = next;
					yield next;
					continue;
				}
				if (server.closed.peek() !== undefined) return;
				await Signal.race(server.queue, server.closed);
			}
		} finally {
			drop();
		}
	}
}

/**
 * A pending request for a broadcast to be served on demand.
 *
 * Yielded by {@link Dynamic.requested}. {@link accept} resolves it with a live
 * broadcast; {@link reject} resolves it with an error. Advancing the iterator or
 * closing it without either rejects the request.
 *
 * @public
 */
export class Request {
	/** The path that was requested. */
	readonly path: Path.Valid;

	#server: ServeState;
	#done = false;

	private constructor(path: Path.Valid, server: ServeState) {
		this.path = path;
		this.#server = server;
	}

	static {
		makeRequest = (path, server) => new Request(path, server);
		finishRequest = (request, err) => {
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

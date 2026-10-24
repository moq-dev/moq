/**
 * A broadcast routing table, independent of any connection.
 *
 * Publish broadcasts into an origin and hand the origin to one or more connections to
 * serve them; the broadcasts outlive any single session. Hand the same (or another)
 * origin to a connection's `consume` option and the peer's announced routes appear
 * in the table too, under whatever its readers watch: each route covers a path prefix,
 * and a request for a path under it resolves through the session that announced it.
 * Mirrors the `origin` module in `rs/moq-net`.
 *
 * @module
 */
import { Derived, type Dispose, type GetPromise, type Getter, getter, Once, race, Signal } from "@moq/signals";
import * as announce from "./announced.ts";
import * as broadcast from "./broadcast.ts";
import type * as Epoch from "./epoch.ts";
import { StreamCode, StreamError } from "./error.ts";
import { Route, routesEqual } from "./hop.ts";
import {
	compareEpochs,
	compareRouteCandidates,
	compareRoutes,
	coveringPrefixes,
	hiddenBelow,
	hooks,
	scopeCaptures,
	scopeHead,
	scopeOverlaps,
} from "./internal.ts";
import * as Path from "./path.ts";
import { Front } from "./resume.ts";
import {
	type Advertised,
	type Advertisements,
	type Instance,
	type InterestListener,
	registerWire,
	sameInstance,
	wireOf,
} from "./wire.ts";

export type { Cost, Hop, Route } from "./hop.ts";
export { isAnonymous } from "./hop.ts";

/**
 * How much an endpoint prefers a link over its others; higher wins. A positive value promotes a
 * link and a negative one demotes it. See {@link Producer.withPreference}.
 */
export type Preference = number & { readonly _brand: "preference" };

/** Constructors for {@link Preference}. */
export const Preference = Object.assign(
	/** Rank a link, throwing a `RangeError` outside -128..=127. */
	(value: number): Preference => {
		if (!Number.isInteger(value) || value < -128 || value > 127) {
			throw new RangeError(`preference must be an integer in -128..=127: ${value}`);
		}
		return value as Preference;
	},
	{
		/** The neutral 0 every link starts at, and what a broadcast published locally ranks. */
		default: 0 as Preference,
	},
);

/** The rooted permissions shared by a handle and every route it inserts. */
class Scope {
	static readonly all = new Scope(Path.empty());

	readonly root: Path.Valid;
	readonly allowed?: Path.Patterns;

	constructor(root: Path.Valid, allowed?: Path.Patterns) {
		this.root = root;
		this.allowed = allowed;
	}

	narrow(root: Path.Valid, patterns: Path.Patterns): Scope {
		const joined = Path.encode(Path.join(this.root, root));
		const rooted = patterns.rooted(joined);
		const allowed = this.allowed?.intersect(rooted) ?? rooted;
		if (allowed.size === 0) throw new Error("origin scopes do not overlap");
		return new Scope(joined, allowed);
	}

	matches(path: Path.Valid): boolean {
		return this.allowed?.matches(path) ?? true;
	}

	path(path: Path.Valid): Path.Valid {
		const joined = Path.encode(Path.join(this.root, path));
		if (!this.matches(joined)) throw new Error("path is outside the origin scope");
		return joined;
	}

	prefix(prefix: Path.Valid): Path.Valid {
		const joined = Path.encode(Path.join(this.root, prefix));
		if (this.allowed && !this.allowed.overlaps(Path.Pattern.subtree(joined))) {
			throw new Error("prefix is outside the origin scope");
		}
		return joined;
	}

	patterns(pattern: Path.Pattern = Path.Pattern.all()): Path.Patterns {
		const rooted = new Path.Patterns([pattern.rooted(this.root)]);
		return this.allowed?.intersect(rooted) ?? rooted;
	}

	/** Omit nested heads because the outer subscription already carries their routes. */
	#heads(): Path.Valid[] {
		if (!this.allowed) return [Path.empty()];
		const heads = [...new Set([...this.allowed.rebase(this.root)].map(scopeHead))].sort();
		return heads.filter((head) => !heads.some((other) => other !== head && Path.hasPrefix(other, head)));
	}

	#headsCache?: Path.Valid[];

	/**
	 * The root-relative prefixes a session asks its peer about for one watched absolute prefix:
	 * a watch above a head requests the head, since this scope may not ask for more, and a
	 * watch inside one requests itself.
	 */
	interests(absolute: Path.Valid): Path.Valid[] {
		if (this === Scope.all) return [absolute];
		this.#headsCache ??= this.#heads();
		const relative = Path.stripPrefix(this.root, absolute);
		if (relative === null) return Path.hasPrefix(absolute, this.root) ? [...this.#headsCache] : [];
		const out: Path.Valid[] = [];
		for (const head of this.#headsCache) {
			if (Path.hasPrefix(relative, head)) out.push(head);
			else if (Path.hasPrefix(head, relative) && this.overlaps(absolute)) return [relative];
		}
		return out;
	}

	/** Whether this scope allows anything beneath the absolute `prefix`. */
	overlaps(prefix: Path.Valid): boolean {
		return this.allowed?.overlaps(Path.Pattern.subtree(prefix)) ?? true;
	}

	/** The exact paths within this scope, relative to its root. */
	projectPaths<T>(values: ReadonlyMap<Path.Valid, T> | undefined): ReadonlyMap<Path.Valid, T> | undefined {
		if (!values || this === Scope.all) return values;
		const out = new Map<Path.Valid, T>();
		for (const [path, value] of values) {
			if (!this.matches(path)) continue;
			const relative = Path.stripPrefix(this.root, path);
			if (relative !== null) out.set(relative, value);
		}
		return out;
	}

	/**
	 * The advertisements that may serve this scope, relative to its root. Every prefix at or
	 * above the root presents as the empty path, most specific first, since that is the order
	 * a request beneath the root resolves in.
	 */
	projectRoutes(values: Advertisements | undefined): Advertisements | undefined {
		if (!values || this === Scope.all) return values;
		const out = new Map<Path.Valid, readonly Advertised[]>();
		const covering: [Path.Valid, Advertised[]][] = [];
		const allowed = this.allowed && [...this.allowed];
		for (const [path, candidates] of values) {
			const relative = Path.stripPrefix(this.root, path);
			const above = relative === null || relative === Path.empty();
			if (above && !Path.hasPrefix(path, this.root)) continue;
			const visible = candidates
				.filter((value) => !allowed || allowed.some((pattern) => advertOverlaps(value, path, pattern)))
				// The claim moves with the key, so it compares against root-relative requests.
				.map((value) => (value.claim ? { ...value, claim: value.claim.rebase(this.root) } : value));
			if (visible.length === 0) continue;
			if (!above) {
				out.set(relative, visible);
				continue;
			}
			// Hold the empty path's place in the order until every covering prefix is known.
			if (covering.length === 0) out.set(Path.empty(), []);
			covering.push([path, visible]);
		}
		if (covering.length > 0) {
			covering.sort(([a], [b]) => b.length - a.length);
			out.set(
				Path.empty(),
				covering.flatMap(([, visible]) => visible),
			);
		}
		return out;
	}
}

/** Whether the route advertised at `prefix` may serve any path `pattern` admits. */
function advertOverlaps(advert: Advertised, prefix: Path.Valid, pattern: Path.Pattern): boolean {
	return advert.claim ? advert.claim.overlaps(pattern) : scopeOverlaps(pattern, prefix);
}

/** One segment of the watched-prefix trie, kept while it or anything beneath it is watched. */
interface WatchNode {
	readonly path: Path.Valid;
	readonly parent?: WatchNode;
	readonly children: Map<string, WatchNode>;
	/** How many open watchers name exactly this prefix. */
	count: number;
}

/** Whether something strictly above `node` is watched, so `node` rides its request. */
function coveredAbove(node: WatchNode): boolean {
	for (let above = node.parent; above; above = above.parent) if (above.count > 0) return true;
	return false;
}

/** The broadest watched prefixes strictly beneath `node`: the ones it covers. */
function frontier(node: WatchNode, out: Path.Valid[] = []): Path.Valid[] {
	for (const child of node.children.values()) {
		if (child.count > 0) out.push(child.path);
		else frontier(child, out);
	}
	return out;
}

/**
 * Presents advertised prefixes relative to a root. Every prefix at or above the root
 * collapses to the empty path, where the most specific one wins, since that is the route
 * a request beneath the root resolves through.
 */
class CoveringRoot {
	readonly #root: Path.Valid;
	#covering?: Path.Valid;

	constructor(root: Path.Valid) {
		this.#root = root;
	}

	/** The presented path for `path`, or undefined when it is outside the root or a broader cover. */
	relative(path: Path.Valid): Path.Valid | undefined {
		const relative = Path.stripPrefix(this.#root, path);
		if (relative !== null && relative !== Path.empty()) return relative;
		if (relative === null && !Path.hasPrefix(path, this.#root)) return undefined;
		if (this.#covering !== undefined && !Path.hasPrefix(this.#covering, path)) return undefined;
		this.#covering = path;
		return Path.empty();
	}
}

/**
 * One requested path: the notify node for everything watching it.
 *
 * `route` is the only reactive part, and the only thing a {@link Request} subscribes to, so
 * a publish or retraction anywhere else in the table cannot wake it. The origin's tables stay
 * the storage; this is a per-path view onto them, refreshed by whichever mutator touched the
 * path. The alternative, deriving each request over the whole `local`/`remote` maps, wakes
 * every open request on every unrelated change.
 *
 * `answer` is one session's: when that session dies the request it resolved ends, since the
 * next session is another publisher instance unless an epoch says otherwise. A slot still
 * waiting is answered by the next session.
 *
 * `handles` holds the `closed` of each open {@link Requesting} on the path.
 *
 * A refusal is terminal, even while another source still serves: the slot ends, every handle
 * closes with the handler's error, and the slot leaves the table, so the next request asks
 * afresh. A refusal never falls through to a broader prefix or another advertiser.
 *
 * @internal
 */
export interface RequestSlot {
	/** Open handles that always take a blind answer. */
	blind: number;
	/** Open announcement-gated handles, which take a blind answer only while discovery is incomplete. */
	announced: number;
	answer?: broadcast.Consumer;
	readonly handles: Set<Once<Error | null>>;
	readonly route: Signal<Resolution | undefined>;
	/** The front a detached slot took from the table, closed with its last handle. */
	retired?: broadcast.Consumer;
	/** The route a detached slot asked to take over under the same epoch, until it answers. */
	failover?: RouteEntry;
}

/** One path resolution and the epoch of the route that can currently serve it. */
interface Resolution {
	readonly front?: broadcast.Consumer;
	readonly epoch?: Epoch.Valid;
	/** The local broadcast or route entry's identity the front came from, when it did. */
	readonly source?: object;
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
	readonly scope: Scope;
	/** The paths the entry may serve beneath its prefix, when its producer is scoped. */
	readonly claim?: Path.Patterns;
	readonly route: Signal<Route>;
	/** The rank of the link the entry arrived on; see {@link Producer.withPreference}. */
	readonly preference: Preference;
	readonly originated: boolean;
	readonly server?: ServeState;
}

/** One advertisement at a prefix. `exact` marks an announced local broadcast, which is only its own path. */
interface Candidate extends Advertised {
	readonly exact: boolean;
	readonly preference: Preference;
}

/**
 * Orders advertisements at a prefix: the better route, then a local broadcast on a tie, then
 * fewer hops. A remaining tie is left to the table's newest-first order; see
 * {@link compareRouteCandidates}.
 */
function compareCandidates(a: Candidate, b: Candidate): number {
	return (
		compareLinks(a.route, b.route, a.preference, b.preference) ||
		compareRoutes(a.route, b.route) ||
		Number(b.exact) - Number(a.exact) ||
		a.route.hops.length - b.route.hops.length
	);
}

/**
 * Orders routes from different links: the newer epoch, since an older one is a publisher that
 * was replaced, then the higher link {@link Preference}, so metrics from different operators are
 * never compared. Zero on a tie, leaving the rest to {@link compareRoutes}.
 */
function compareLinks(a: Route, b: Route, aPreference: Preference, bPreference: Preference): number {
	return compareEpochs(a, b) || bPreference - aPreference;
}

/**
 * The preferred of `entries` (newest first, at `prefix`) for resolving `path`, not skipped:
 * see {@link compareRouteCandidates}, then newest.
 */
function preferredEntry(
	path: Path.Valid,
	prefix: Path.Valid,
	entries: readonly RouteEntry[],
	skip?: (entry: RouteEntry) => boolean,
): RouteEntry | undefined {
	let best: RouteEntry | undefined;
	for (const entry of entries) {
		if (skip?.(entry)) continue;
		if (!best) {
			best = entry;
			continue;
		}
		const a = entry.route.peek();
		const b = best.route.peek();
		const order =
			compareLinks(a, b, entry.preference, best.preference) || compareRouteCandidates(path, prefix, a, b);
		if (order < 0) best = entry;
	}
	return best;
}

/** Whether a session received `entry`, so it is never forwarded to a peer. */
function received(entry: RouteEntry): boolean {
	return !entry.originated;
}

function unroutable(): StreamError {
	return new StreamError(StreamCode.Unroutable, { message: "unroutable" });
}

/** A served route from {@link Producer.dynamic}: the queue a handler drains. */
class ServeState {
	readonly root: Path.Valid;
	/** The prefix the route claims; every served path is at or beneath it. */
	readonly prefix: Path.Valid;
	/** False when a session announced the route: it may serve that session's broadcast. */
	readonly originated: boolean;

	constructor(root: Path.Valid, prefix: Path.Valid, originated: boolean) {
		this.root = root;
		this.prefix = prefix;
		this.originated = originated;
	}

	queue = new Signal(new Set<Request>());
	pending = new Map<Path.Valid, Request>();
	served = new Map<Path.Valid, broadcast.Consumer>();
	rejected = new Map<Path.Valid, Error>();
	// demand() is the only reader of `rejected`. A Consumer.request refusal never
	// re-enqueues, so storing the error without a waiter would pin every unique
	// path until the route dies.
	demanding = new Map<Path.Valid, number>();
	closed = new Once<Error | null>();
	settled = new Signal(0);
	/** Counts {@link reset}s, so a waiter can tell its request was released rather than answered. */
	resets = 0;
	onChange: (path: Path.Valid) => void = () => {};
	onReject: (path: Path.Valid, err: Error) => void = () => {};

	enqueue(path: Path.Valid): void {
		if (this.closed.peek() !== undefined) return;
		this.rejected.delete(path);
		if (this.pending.has(path)) return;
		const live = this.served.get(path);
		if (live && live.closed.peek() === undefined) return;
		const request = makeRequest(Path.stripPrefix(this.root, path) ?? Path.empty(), this);
		this.pending.set(path, request);
		this.queue.mutate((queue) => {
			queue.add(request);
		});
	}

	/** Withdraw only this pending request; a later answer cannot replace a newer one. */
	forget(path: Path.Valid, request: Request): void {
		if (this.pending.get(path) !== request) return;
		this.pending.delete(path);
		this.queue.mutate((queue) => {
			queue.delete(request);
		});
		this.rejected.delete(path);
		finishRequest(request, unroutable());
	}

	accept(request: Request, front: broadcast.Consumer): void {
		const path = Path.join(this.root, request.path);
		if (this.closed.peek() !== undefined || this.pending.get(path) !== request) {
			front.close();
			return;
		}
		this.pending.delete(path);
		const existing = this.served.get(path);
		if (existing && existing.closed.peek() === undefined) {
			front.close();
			this.onChange(path);
			this.settled.update((n) => n + 1);
			return;
		}
		// A route claiming a prefix names no broadcast, so a path it serves on request has no
		// inventory to list.
		if (path !== this.prefix) hooks.stampUnlisted(front);
		this.served.set(path, front);
		void front.closed.then(() => {
			if (this.served.get(path) !== front) return;
			this.served.delete(path);
			this.onChange(path);
		});
		this.onChange(path);
		this.settled.update((n) => n + 1);
	}

	has(request: Request): boolean {
		return this.closed.peek() === undefined && this.pending.get(Path.join(this.root, request.path)) === request;
	}

	reject(request: Request, err: Error): void {
		const path = Path.join(this.root, request.path);
		if (this.pending.get(path) !== request) return;
		this.pending.delete(path);
		if (this.demanding.has(path)) this.rejected.set(path, err);
		this.onReject(path, err);
		this.settled.update((n) => n + 1);
	}

	close(abort?: Error): void {
		if (this.closed.peek() !== undefined) return;
		const err = abort ?? unroutable();
		this.closed.set(err);
		this.reset(err);
	}

	/**
	 * Release answers and the requests handlers hold when the publisher instance changes: a
	 * handler's late answer is for the old one, and the requesters ask the new one again.
	 */
	reset(err: Error = unroutable()): void {
		this.resets++;
		const queued = [...this.pending.values()];
		this.pending.clear();
		this.queue.mutate((queue) => {
			queue.clear();
		});
		for (const request of queued) {
			finishRequest(request, err);
		}
		const served = [...this.served];
		this.served.clear();
		for (const [path, front] of served) {
			front.close();
			this.onChange(path);
		}
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

// Navigation belongs to relative references, resolved before publication.
function publishPath(path: Path.Valid): void {
	if (Path.parts(path).some((segment) => segment === "." || segment === "..")) {
		throw new Error("broadcast path contains a navigation segment");
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
	#snapshot: ReadonlyMap<Path.Valid, readonly Candidate[]> = new Map();

	/** Build one candidate table per mutation, shared by announcement watchers. */
	snapshot(): ReadonlyMap<Path.Valid, readonly Candidate[]> {
		const version = `${this.local.version}/${this.advertisedLocal.version}/${this.routes.version}`;
		if (version !== this.#snapshotVersion) {
			this.#snapshot = this.candidates();
			this.#snapshotVersion = version;
		}
		return this.#snapshot;
	}

	/**
	 * Every advertisement per prefix, most preferred first, without the `skip`ped entries.
	 * Readers select after filtering by their scope, so a cheaper route they cannot see
	 * never hides one they can.
	 */
	candidates(skip?: (entry: RouteEntry) => boolean): Map<Path.Valid, Candidate[]> {
		const out = new Map<Path.Valid, Candidate[]>();
		for (const [path, entries] of this.routes.peek() ?? []) {
			const list: Candidate[] = [];
			for (const entry of entries) {
				if (skip?.(entry)) continue;
				list.push({
					identity: entry.identity,
					route: entry.route.peek(),
					claim: entry.claim,
					exact: false,
					preference: entry.preference,
				});
			}
			if (list.length > 0) out.set(path, list);
		}
		const advertised = this.advertisedLocal.peek();
		for (const [path, front] of this.local.peek() ?? []) {
			const local = {
				identity: front,
				route: advertised?.get(path) ?? Route.default,
				exact: true,
				preference: Preference.default,
			};
			const list = out.get(path);
			if (list) list.push(local);
			else out.set(path, [local]);
		}
		for (const list of out.values()) {
			// Stable, so equal routes keep the table's newest-first order.
			if (list.length > 1) list.sort(compareCandidates);
		}
		return out;
	}

	// Originated advertisements sessions should forward: exact-path announces plus
	// originated dynamics. Identity is the local front or the route entry, so a
	// republish diffs as retract-then-announce and a re-price as another active.
	originated = new Signal<Advertisements | undefined>(new Map());

	// Broadcasts materialized from a served route, keyed by exact path. Shared by every
	// request for the path so repeats reuse one accept; dropped (and closed) when the
	// providing route goes away or the last request releases it.
	materialized = new Map<
		Path.Valid,
		{ entry: RouteEntry; front: broadcast.Consumer; epoch?: Epoch.Valid; source: object }
	>();

	// Paths consumers asked for without waiting for an announcement; attached sessions
	// answer them with blind subscriptions. Never announced: an answered request is assumed
	// present, not known live, so it must not read as an availability claim.
	requests = new Signal<Map<Path.Valid, RequestSlot> | undefined>(new Map());

	// Path-local wakeups for announced requests, which survive the end of a resolved instance.
	// A counter rather than the instance: the instance is a plain object, so two epochless
	// routes with identical metadata compare equal, and a coalesced write could hide a takeover
	// or the origin closing. Watchers read the table and `closed` themselves when woken.
	watchers = new Map<Path.Valid, { users: number; version: Signal<number> }>();

	// Slots taken out of `requests` because another instance won their path, still open for
	// their handles. Nothing joins them, but they are refreshed with the table, so one ends
	// once its own instance stops serving, except retained answers from an in-place Restart.
	detached = new Map<Path.Valid, Set<RequestSlot>>();

	// An in-place Restart forgets served answers for future requests, but resolved handles
	// retain their old broadcast without participating in routing or same-epoch failover.
	retained = new WeakSet<RequestSlot>();

	// How many sessions are attached, and how many of those support broadcast discovery.
	// What backs the public `discovery` getter.
	sessions = new Signal({ total: 0, discovery: 0 });

	// The prefixes whose initial announcements a session is still delivering, counted. An
	// announcement stream overlapping one has not synced yet.
	snapshots = new Signal(new Map<Path.Valid, number>());

	/** Hold an initial set in flight under the absolute `prefix` until the returned dispose. */
	addSnapshot(prefix: Path.Valid): Dispose {
		this.snapshots.mutate((counts) => {
			counts.set(prefix, (counts.get(prefix) ?? 0) + 1);
		});
		let landed = false;
		return () => {
			if (landed) return;
			landed = true;
			this.snapshots.mutate((counts) => {
				const count = (counts.get(prefix) ?? 0) - 1;
				if (count > 0) counts.set(prefix, count);
				else counts.delete(prefix);
			});
		};
	}

	// The literal heads readers are watching, counted, as a trie: announcement streams and
	// announced requests. Sessions ask their peer for announcements under the broadest of
	// these and nothing else, and hear each change to that covering set as it happens, so a
	// watch coming or going touches only its prefix and the watches directly above or below.
	#watches: WatchNode = { path: Path.empty(), children: new Map(), count: 0 };
	#interests = new Set<InterestListener>();

	/** Count `heads` as watched until the returned dispose runs. */
	watch(heads: readonly Path.Valid[]): Dispose {
		for (const head of heads) this.#watchOne(head);
		let released = false;
		return () => {
			if (released) return;
			released = true;
			for (const head of heads) this.#unwatchOne(head);
		};
	}

	#watchOne(head: Path.Valid): void {
		let node = this.#watches;
		for (const part of head === Path.empty() ? [] : head.split("/")) {
			let child = node.children.get(part);
			if (!child) {
				child = { path: Path.join(node.path, part as Path.Valid), parent: node, children: new Map(), count: 0 };
				node.children.set(part, child);
			}
			node = child;
		}
		if (node.count++ > 0 || coveredAbove(node)) return;
		// Opened before the narrower ones it covers are released, so nothing retracts.
		this.#emit(node.path, true);
		for (const narrower of frontier(node)) this.#emit(narrower, false);
	}

	#unwatchOne(head: Path.Valid): void {
		let node: WatchNode | undefined = this.#watches;
		for (const part of head === Path.empty() ? [] : head.split("/")) node = node?.children.get(part);
		if (!node || node.count === 0) throw new Error("unwatched prefix released");
		if (--node.count === 0 && !coveredAbove(node)) {
			for (const narrower of frontier(node)) this.#emit(narrower, true);
			this.#emit(node.path, false);
		}
		// Prune the segments nothing watches any more.
		while (node.parent && node.count === 0 && node.children.size === 0) {
			node.parent.children.delete(node.path.slice(node.path.lastIndexOf("/") + 1));
			node = node.parent;
		}
	}

	#emit(prefix: Path.Valid, wanted: boolean): void {
		for (const listener of this.#interests) listener(prefix, wanted);
	}

	/**
	 * Hear every change to the covering set of watches, starting with each prefix in it now.
	 * Returns the unsubscribe.
	 */
	interests(listener: InterestListener): Dispose {
		this.#interests.add(listener);
		if (this.#watches.count > 0) listener(this.#watches.path, true);
		else for (const prefix of frontier(this.#watches)) listener(prefix, true);
		return () => this.#interests.delete(listener);
	}

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
		this.watchers.get(path)?.version.update((n) => n + 1);
		const slot = this.requests.peek()?.get(path);
		const detached = [...(this.detached.get(path) ?? [])];
		if (slot) this.reroute(path, slot);
		for (const other of detached) this.reroute(path, other);
		this.forget(path);
	}

	/** Release unanswered requests no wire waiter or local route still needs. */
	forget(path: Path.Valid): void {
		const slot = this.requests.peek()?.get(path);
		// An empty slot remains joinable until its deferred teardown removes it.
		const best = slot ? this.bestEntry(path) : undefined;
		const wanted = best && !this.localWins(path, best) ? best.server : undefined;
		const failovers = this.detached.get(path);
		let held: Set<RouteEntry> | undefined;
		for (const prefix of coveringPrefixes(path)) {
			for (const entry of this.routes.peek()?.get(prefix) ?? []) {
				const server = entry.server;
				const pending = server?.pending.get(path);
				if (!server || !pending || server.demanding.has(path) || server === wanted) continue;
				if (failovers) {
					// Build once, only when pending work needs a failover ownership check.
					if (!held) {
						held = new Set();
						for (const slot of failovers) {
							if (slot.handles.size > 0 && slot.failover) held.add(slot.failover);
						}
					}
					if (held.has(entry)) continue;
				}
				server.forget(path, pending);
			}
		}
	}

	/**
	 * Recompute what `slot` resolves to. A slot never moves to another publisher instance: while
	 * its own still serves and another wins `path`, it keeps what it resolved for the handles
	 * already on it and the next request resolves the winner on a slot of its own; once its own
	 * stops serving, it ends, except for an answer retained across an in-place Restart.
	 */
	reroute(path: Path.Valid, slot: RequestSlot): void {
		if (this.retained.has(slot)) return;
		const current = slot.route.peek();
		if (current?.front && current.source) {
			if (this.detached.get(path)?.has(slot)) {
				this.failover(path, slot, current.source, current.epoch);
				return;
			}
			if (!this.serves(path, current.source, current.epoch, false)) {
				this.drop(path, slot, unroutable());
				return;
			}
			const next = this.instance(path);
			const held = { identity: current.source, route: { epoch: current.epoch } };
			if (next && !sameInstance(held, next)) {
				this.retire(path, slot);
				return;
			}
		}
		slot.route.set(this.route(path, slot));
	}

	/**
	 * Keep a detached slot on its instance: on its own source while that serves, else on another
	 * route of the same epoch, which serves the same bytes, keeping the old front until that one
	 * answers. Without such a route the slot ends.
	 */
	failover(path: Path.Valid, slot: RequestSlot, source: object, epoch: Epoch.Valid | undefined): void {
		if (this.serves(path, source, epoch, true)) {
			// Its own source serves again: a takeover still pending speaks for nobody.
			slot.failover = undefined;
			return;
		}
		// A local broadcast announced under the epoch serves the same bytes with no round trip.
		const local = this.local.peek()?.get(path);
		if (epoch !== undefined && local && this.advertisedLocal.peek()?.get(path)?.epoch === epoch) {
			slot.failover = undefined;
			slot.retired?.close();
			slot.retired = undefined;
			slot.route.set({ front: local, epoch, source: local });
			return;
		}
		const entry =
			epoch === undefined
				? undefined
				: this.bestEntry(path, (candidate) => !candidate.server || candidate.route.peek().epoch !== epoch);
		if (!entry?.server) {
			this.drop(path, slot, unroutable());
			return;
		}
		const served = entry.server.served.get(path);
		if (!served || served.closed.peek() !== undefined) {
			slot.failover = entry;
			entry.server.enqueue(path);
			return;
		}
		slot.failover = undefined;
		slot.retired?.close();
		slot.retired = served;
		slot.route.set({ front: served, epoch, source: entry.identity });
	}

	/**
	 * Whether the instance `source` served under `epoch` still serves `path`. Under an epoch any
	 * route of it does, unless `exact` asks for `source` itself; without one only `source` does,
	 * since nothing says another serves the same bytes.
	 */
	serves(path: Path.Valid, source: object, epoch: Epoch.Valid | undefined, exact: boolean): boolean {
		const matches = (identity: object) => identity === source || (!exact && epoch !== undefined);
		const local = this.local.peek()?.get(path);
		const advertised = this.advertisedLocal.peek()?.get(path);
		if (local && advertised && advertised.epoch === epoch && matches(local)) return true;
		for (const [prefix, entries] of this.routes.peek() ?? []) {
			if (!Path.hasPrefix(prefix, path)) continue;
			for (const entry of entries) {
				if (!entry.server || !entry.scope.matches(path) || entry.route.peek().epoch !== epoch) continue;
				if (matches(entry.identity)) return true;
			}
		}
		return false;
	}

	/** The publisher instance a request for `path` resolves through, when one serves it. */
	instance(path: Path.Valid): Instance | undefined {
		const entry = this.bestEntry(path);
		const local = this.local.peek()?.get(path);
		if (local && this.localWins(path, entry)) {
			return { identity: local, route: { epoch: this.advertisedLocal.peek()?.get(path)?.epoch } };
		}
		if (!entry?.server) return undefined;
		return { identity: entry.identity, route: entry.route.peek() };
	}

	/**
	 * Take `slot` out of the table with what it resolved: its handles stay on that instance
	 * until they close, and nothing joins it again.
	 */
	retire(path: Path.Valid, slot: RequestSlot): void {
		this.requests.mutate((map) => {
			if (map?.get(path) === slot) map.delete(path);
		});
		let detached = this.detached.get(path);
		if (!detached) {
			detached = new Set();
			this.detached.set(path, detached);
		}
		detached.add(slot);
		const cached = this.materialized.get(path);
		if (cached && cached.front === slot.route.peek()?.front) {
			this.materialized.delete(path);
			slot.retired = cached.front;
		}
	}

	/** Preserve this route's resolved answers before its server resets for another epoch. */
	restart(entry: RouteEntry): void {
		for (const path of entry.server?.served.keys() ?? []) {
			const joined = this.requests.peek()?.get(path);
			const slots = [...(this.detached.get(path) ?? [])];
			if (joined) slots.push(joined);
			for (const slot of slots) {
				const current = slot.route.peek();
				if (
					!current?.front ||
					current.front.closed.peek() !== undefined ||
					current.source !== entry.identity ||
					this.retained.has(slot)
				)
					continue;
				this.retire(path, slot);
				// reset() releases the server's front; retain a separate owner even when no
				// request has read active yet and therefore no per-request clone exists.
				slot.retired = current.front.clone();
				slot.failover = undefined;
				this.retained.add(slot);
			}
		}
	}

	/**
	 * Whether sessions should answer `slot` with a blind subscription. An announcement-gated
	 * handle falls back to one only while at least one attached session cannot announce, and
	 * stays gated with no session attached.
	 */
	blind(slot: RequestSlot): boolean {
		if (slot.blind > 0) return true;
		if (slot.announced === 0) return false;
		const { total, discovery } = this.sessions.peek();
		return discovery < total;
	}

	/**
	 * `entry` refused `path` with `err`: the request ends with `err`, even while another
	 * source serves, since asking the next candidate would turn one refusal into a request
	 * per candidate.
	 */
	refuse(path: Path.Valid, entry: RouteEntry, err: Error): void {
		// A detached slot failing over to this route ends with its answer.
		for (const other of [...(this.detached.get(path) ?? [])]) {
			if (other.failover === entry) this.drop(path, other, err);
		}
		const slot = this.requests.peek()?.get(path);
		if (!slot) return;
		// Only the route the request is waiting on speaks for it; one superseded by another
		// route or a local broadcast has a moot answer.
		if (this.bestEntry(path) !== entry || this.localWins(path, entry)) return;
		this.drop(path, slot, err);
	}

	/** End `slot` with `err`, wherever it is held: it leaves the table and every handle closes. */
	drop(path: Path.Valid, slot: RequestSlot, err: Error): void {
		const joined = this.requests.peek()?.get(path) === slot;
		if (joined) {
			this.requests.mutate((map) => {
				map?.delete(path);
			});
		}
		this.detach(path, slot);
		slot.failover = undefined;
		slot.answer?.close();
		slot.answer = undefined;
		slot.route.set(undefined);
		if (joined) this.releaseMaterialized(path);
		this.forget(path);
		for (const closed of slot.handles) closed.set(err);
		slot.handles.clear();
	}

	/** Forget a detached slot, releasing the front it took. */
	detach(path: Path.Valid, slot: RequestSlot): void {
		const detached = this.detached.get(path);
		if (!detached?.delete(slot)) return;
		if (detached.size === 0) this.detached.delete(path);
		slot.retired?.close();
		slot.retired = undefined;
	}

	/**
	 * Recompute every open request covered by `prefix`, after a route was inserted or
	 * removed there: a route covers many paths, so a single-path refresh is not enough.
	 * Every materialized broadcast belongs to an open request, so rerouting them also
	 * releases a retracted route's session subscription even when nothing reads it again.
	 */
	refreshPrefix(prefix: Path.Valid): void {
		for (const [path, watch] of this.watchers) if (Path.hasPrefix(prefix, path)) watch.version.update((n) => n + 1);
		const detached = [...this.detached].map(([path, slots]) => [path, [...slots]] as const);
		for (const [path, slot] of [...(this.requests.peek() ?? [])]) {
			if (Path.hasPrefix(prefix, path)) this.reroute(path, slot);
		}
		for (const [path, slots] of detached) {
			if (!Path.hasPrefix(prefix, path)) continue;
			for (const slot of slots) this.reroute(path, slot);
		}
		for (const path of this.requests.peek()?.keys() ?? []) if (Path.hasPrefix(prefix, path)) this.forget(path);
		for (const [path] of detached) if (Path.hasPrefix(prefix, path)) this.forget(path);
	}

	/** Rebuild the publisher-facing originated table after an advertisement write. */
	rebuildOriginated(): void {
		if (!this.local.peek() && !this.advertisedLocal.peek() && !this.routes.peek()) {
			this.originated.set(undefined);
			return;
		}
		// A local broadcast and an originated dynamic at one path compete on cost, as they do for requests.
		this.originated.set(this.candidates(received));
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
		const routes = this.routes.peek();
		if (!routes) return undefined;
		for (const prefix of coveringPrefixes(path)) {
			const entries = routes.get(prefix);
			if (!entries) continue;
			const entry = preferredEntry(
				path,
				prefix,
				entries,
				(candidate) => !candidate.scope.matches(path) || (skip?.(candidate) ?? false),
			);
			if (entry) return entry;
		}
		return undefined;
	}

	/**
	 * Whether the announced local broadcast at `path` wins over `entry`, the best route a
	 * session or dynamic handle announced there. They compare as any two routes do, with the
	 * local broadcast at {@link Preference.default}: an identified route strictly cheaper
	 * than the local one wins, and the local broadcast wins a tie. A route at a shorter prefix never competes, since the most specific
	 * prefix wins outright. False when nothing is announced locally at `path`.
	 */
	localWins(path: Path.Valid, entry: RouteEntry | undefined): boolean {
		const local = this.advertisedLocal.peek()?.get(path);
		if (!local || !this.local.peek()?.has(path)) return false;
		if (!entry || !this.routes.peek()?.get(path)?.includes(entry)) return true;
		const remote = entry.route.peek();
		return (compareLinks(local, remote, Preference.default, entry.preference) || compareRoutes(local, remote)) <= 0;
	}

	/**
	 * What `path` resolves to: an announced local publish, a broadcast materialized from
	 * the best covering route, or the blind answer.
	 *
	 * Materialization is lazy and cached per path: the first request under a route opens
	 * the providing session's subscription and repeats share it. A better route is made
	 * before the old one breaks: the current front keeps serving until the new route
	 * answers (then swaps) or refuses (then the request ends). A retracted route swaps at once.
	 */
	route(path: Path.Valid, slot: Pick<RequestSlot, "answer">): Resolution | undefined {
		const entry = this.bestEntry(path);
		const local = this.local.peek()?.get(path);
		if (local && this.localWins(path, entry)) {
			// Nothing reads a remote front the local broadcast replaced, so close its session subscription.
			this.releaseMaterialized(path);
			return { front: local, epoch: this.advertisedLocal.peek()?.get(path)?.epoch, source: local };
		}

		let cached = this.materialized.get(path);
		if (cached && cached.front.closed.peek() !== undefined) {
			this.materialized.delete(path);
			cached = undefined;
		}
		const epoch = entry?.route.peek().epoch;
		if (cached && cached.entry === entry && cached.epoch === epoch) return cached;
		if (!entry?.server) {
			this.releaseMaterialized(path);
			return slot.answer ? { front: slot.answer } : undefined;
		}

		const served = entry.server.served.get(path);
		if (served && served.closed.peek() === undefined) {
			cached?.front.close();
			const resolution = { entry, front: served, epoch, source: entry.identity };
			this.materialized.set(path, resolution);
			return resolution;
		}

		entry.server.enqueue(path);
		// The same instance over another route: the old front serves until the new one answers.
		const kept = cached?.epoch === epoch ? cached : undefined;
		return { front: kept?.front, epoch, source: kept?.source };
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

	/** The available broadcasts under `scope`, as a live stream; see {@link Consumer.announced}. */
	announced(scope?: Path.Pattern, options?: announce.Options): announce.Consumer;

	/** Advertise a prefix and serve requests under it; see {@link Producer.dynamic}. */
	dynamic(prefix: Path.Valid): Dynamic;

	/** Insert a route with protocol metadata. @internal */
	dynamicRoute(
		prefix: Path.Valid,
		route?: Route | { epoch?: Route["epoch"]; hops?: Route["hops"]; cost?: Route["cost"]; auth?: Route["auth"] },
	): Dynamic;
}

/** Options for resolving a broadcast path. */
export interface RequestOptions {
	/**
	 * Follow announced instances unless an epoch is pinned; resolve blindly when discovery is
	 * unavailable. Watches the path, so a `consume` session asks its peer about it.
	 */
	announced?: boolean;
	/** Refuse a different publisher instance, including an asynchronous answer or a retry. */
	epoch?: Epoch.Valid;
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
	#scope = Scope.all;
	#preference = Preference.default;
	#requests?: Getter<ReadonlyMap<Path.Valid, RequestSlot> | undefined>;

	// Set on a derived handle, which borrows the origin: what it registered, so its close
	// releases only that. The owner has none, since its close ends the whole origin.
	#borrowed?: { releases: Set<Dispose>; closed: boolean };

	// The reader backing the passthroughs, so holding a Producer never requires the
	// consume().x() stutter for everyday reads. One instance, so `discovery` keeps its
	// identity across reads.
	#reader = makeConsumer(this.#state, this.#scope);

	constructor() {
		const thisProducer = this;
		registerWire(this, {
			receive: (prefix, route) => this.#receive(prefix, route),
			interests: (listener) =>
				this.#state.interests((absolute, wanted) => {
					for (const prefix of this.#scope.interests(absolute)) listener(prefix, wanted);
				}),
			accepts: (prefix) => this.#scope.overlaps(Path.join(this.#scope.root, prefix)),
			attach: (discovery) => this.#attach(discovery),
			expect: () => this.#expect(),
			snapshot: (prefix) =>
				this.#borrowed?.closed ? () => {} : this.#hold(this.#state.addSnapshot(this.#scope.prefix(prefix))),
			get requests() {
				if (thisProducer.#scope === Scope.all) return thisProducer.#state.requests;
				thisProducer.#requests ??= new Derived([thisProducer.#state.requests], (requests) =>
					thisProducer.#scope.projectPaths(requests),
				);
				return thisProducer.#requests;
			},
			changed: () => this.#changed(),
			blind: (slot) => this.#state.blind(slot),
			answer: (path, front) => this.#answer(this.#scope.path(path), front),
			routes: (path) => wireOf(this.#reader).routes(path),
		});
	}

	/**
	 * Narrow this handle to patterns beneath root, presenting paths relative to that root.
	 * The result borrows the origin; see {@link close}.
	 */
	scope(root: Path.Valid, patterns: Path.Patterns): Producer {
		return this.#derive(this.#scope.narrow(root, patterns), this.#preference);
	}

	/**
	 * Rank the link this handle (and any handle derived from it) is given to: higher wins, and
	 * every handle starts at the neutral {@link Preference.default}.
	 *
	 * Applies to what {@link dynamic} advertises, which is how a session lands its peer's
	 * routes, not to a broadcast published here. A route with a newer epoch still wins, but
	 * among the rest preference decides before cost, so links to different operators rank by
	 * this instead of comparing their metrics. Hand each CDN's session its own rank to use the
	 * primary while it has a route and fail over to the next. Mirrors
	 * `origin::Producer::with_preference` in rs/moq-net.
	 */
	withPreference(preference: Preference): Producer {
		return this.#derive(this.#scope, preference);
	}

	// A handle borrowing this origin: closing it releases what it registered, never the origin.
	#derive(scope: Scope, preference: Preference): Producer {
		if (this.#borrowed?.closed) throw new Error("origin handle is closed");
		const producer = new Producer();
		producer.#state = this.#state;
		producer.#scope = scope;
		producer.#preference = preference;
		producer.#reader = makeConsumer(this.#state, scope);
		producer.#borrowed = { releases: new Set(), closed: false };
		return producer;
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
	 * application still holds a consumer clone. Announce with a {@link Route.epoch}
	 * (`Epoch.mint()` per run) so a restart replaces the old broadcast rather than resuming
	 * into it; at the same epoch, a local broadcast competes on cost and wins ties.
	 */
	createBroadcast(path: Path.Valid): broadcast.Producer {
		path = this.#scope.path(path);
		if (this.#borrowed?.closed) throw new Error("origin handle is closed");
		publishPath(path);
		const created = this.#state.created;
		if (!created) throw new Error("origin is closed");

		const producer = new broadcast.Producer();
		hooks.stampPath(producer, path);
		const front = producer.consume();

		hooks.attachAnnouncer(producer, {
			announce: (route) => this.#advertiseExact(path, front, route),
			unannounce: () => this.#retractExact(path, front),
			route: () =>
				this.#state.local.peek()?.get(path) === front
					? this.#state.advertisedLocal.peek()?.get(path)
					: undefined,
		});

		const previous = created.get(path);
		created.set(path, front);
		if (previous) {
			this.#retractExact(path, previous);
			previous.close();
		}

		// Drop it when the broadcast closes, unless a recreate already replaced it: a stale
		// broadcast closing must not unpublish the live one. Clearing `created` also stops a
		// later announce, since a consumer clone can keep the broadcast open past this close.
		const release = this.#hold(() => {
			this.#retractExact(path, front);
			if (this.#state.created?.get(path) === front) this.#state.created.delete(path);
			front.close();
		});
		void front.closed.then(release);

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
	dynamic(prefix: Path.Valid): Dynamic {
		return this.#insertRoute(prefix, Route.default, true, true);
	}

	/** Insert an originated claim with route metadata that answers through `accept`, for tests and benches. @internal */
	dynamicRoute(
		prefix: Path.Valid,
		route:
			| Route
			| {
					epoch?: Route["epoch"];
					hops?: Route["hops"];
					cost?: Route["cost"];
					auth?: Route["auth"];
			  } = Route.default,
	): Dynamic {
		return this.#insertRoute(prefix, Route.normalize(route), true, false);
	}

	/**
	 * Land a route a peer announced, served through the returned handle. Same as
	 * {@link dynamic} but not originated, so a session never announces it back.
	 *
	 * @internal
	 */
	#receive(
		prefix: Path.Valid,
		route:
			| Route
			| {
					epoch?: Route["epoch"];
					hops?: Route["hops"];
					cost?: Route["cost"];
					auth?: Route["auth"];
			  } = Route.default,
	): Dynamic {
		return this.#insertRoute(prefix, Route.normalize(route), false, false);
	}

	#insertRoute(prefix: Path.Valid, route: Route, originated: boolean, publishes: boolean): Dynamic {
		prefix = this.#scope.prefix(prefix);
		publishPath(prefix);
		const server = new ServeState(this.#scope.root, prefix, originated);
		server.onChange = (path) => this.#state.refresh(path);
		const entry: RouteEntry = {
			identity: {},
			scope: this.#scope,
			claim: this.#scope.allowed?.intersect(new Path.Patterns([Path.Pattern.subtree(prefix)])),
			route: new Signal(route),
			preference: this.#preference,
			originated,
			server,
		};
		server.onReject = (path, err) => this.#state.refuse(path, entry, err);

		let closed = this.#borrowed?.closed ?? false;
		if (!closed) {
			this.#state.routes.mutate((routes) => {
				if (!routes) {
					closed = true;
					return;
				}
				const entries = routes.get(prefix);
				if (entries) entries.unshift(entry);
				else routes.set(prefix, [entry]);
			});
		}
		if (closed) {
			server.close();
			return makeDynamic(prefix, entry, this.#state, () => {}, publishes ? this : undefined);
		}
		this.#state.rebuildOriginated();
		this.#state.refreshPrefix(prefix);

		const retract = this.#hold(() => {
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
		});

		return makeDynamic(prefix, entry, this.#state, retract, publishes ? this : undefined);
	}

	/**
	 * Register an attached session, counting it toward the `discovery` state. Returns the
	 * detach; call it exactly once when the session dies.
	 *
	 * @internal
	 */
	#attach(discovery: boolean): Dispose {
		if (this.#borrowed?.closed) return () => {};
		this.#sessions(1, discovery);
		const release = this.#expect();
		return this.#hold(() => {
			this.#sessions(-1, discovery);
			release();
		});
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
		if (this.#borrowed?.closed) return () => {};
		this.#state.answerers.update((count) => count + 1);
		// Clamped because closing the origin zeroes the count, and the sessions attached at
		// the time still release afterwards.
		return this.#hold(() => this.#state.answerers.update((count) => Math.max(0, count - 1)));
	}

	// Run `release` once, at the first of its own call or this derived handle's close, so a
	// borrower's close withdraws what it registered. The owner's close tears down everything.
	#hold(release: Dispose): Dispose {
		const borrowed = this.#borrowed;
		let done = false;
		const held = () => {
			if (done) return;
			done = true;
			borrowed?.releases.delete(held);
			release();
		};
		borrowed?.releases.add(held);
		return held;
	}

	/**
	 * Resolves once anything a serving session scans changes: the open requests, either
	 * side of the routing table, or the attached sessions that decide which requests are blind.
	 *
	 * @internal
	 */
	#changed(): GetPromise<unknown> {
		return Signal.race(
			this.#state.requests,
			this.#state.local,
			this.#state.routes,
			this.#state.advertisedLocal,
			this.#state.sessions,
		);
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
		if (!slot || slot.answer !== undefined || this.#borrowed?.closed) {
			front.close();
			return undefined;
		}
		slot.answer = front;
		this.#state.refresh(path);

		return this.#hold(() => {
			if (slot.answer === front) {
				// A request the answer resolved ends with its session: the next session is another
				// publisher instance, since nothing (no epoch) says it serves the same bytes.
				if (slot.route.peek()?.front === front) {
					this.#state.drop(path, slot, unroutable());
				} else {
					slot.answer = undefined;
					this.#state.refresh(path);
					// The route signal only reaches this path's requesters; poke the map so every
					// serving loop re-scans and one of them re-answers.
					this.#state.requests.mutate(() => {});
				}
			}
			front.close();
		});
	}

	/** A read handle for this origin, the side a connection's `publish` option borrows. */
	consume(): Consumer {
		return makeConsumer(this.#state, this.#scope);
	}

	/** Whether every attached session announces into the table; see {@link Consumer.discovery}. */
	get discovery(): Getter<boolean | undefined> {
		return this.#reader.discovery;
	}

	/** Resolve `path`, optionally waiting for an announcement; see {@link Consumer.request}. */
	request(path: Path.Valid, options?: RequestOptions): Requesting {
		return this.#reader.request(path, options);
	}

	/** The available broadcasts under `scope`, as a live stream; see {@link Consumer.announced}. */
	announced(scope?: Path.Pattern, options?: announce.Options): announce.Consumer {
		return this.#reader.announced(scope, options);
	}

	/**
	 * Close the origin, every broadcast it still routes, and its announcement streams. Idempotent.
	 *
	 * A handle from {@link scope} or {@link withPreference} borrows the origin: closing it
	 * withdraws only the broadcasts, routes, and sessions (with their answers) registered
	 * through it and refuses new ones, ignoring `abort`. Only the handle made with `new Producer()` closes the origin.
	 */
	close(abort?: Error) {
		const borrowed = this.#borrowed;
		if (borrowed) {
			if (borrowed.closed) return;
			borrowed.closed = true;
			for (const release of [...borrowed.releases]) release();
			borrowed.releases.clear();
			return;
		}
		if (this.#state.closed.peek() !== undefined) return;
		this.#state.closed.set(abort ?? null);
		for (const watch of this.#state.watchers.values()) watch.version.update((n) => n + 1);
		for (const front of this.#state.created?.values() ?? []) {
			front.close();
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
		for (const [path, slots] of [...this.#state.detached]) {
			for (const slot of slots) {
				this.#state.detach(path, slot);
				slot.route.set(undefined);
			}
		}
	}
}

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported OriginState. Assigned in the class's static block.
let makeConsumer: (state: OriginState, scope: Scope) => Consumer;

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

let makeDynamic: (
	prefix: Path.Valid,
	entry: RouteEntry,
	state: OriginState,
	retract: Dispose,
	publisher?: Producer,
) => Dynamic;

let makeRequest: (path: Path.Valid, server: ServeState) => Request;
let finishRequest: (request: Request, err: Error) => void;

/**
 * An owned request for a broadcast path; see {@link Consumer.request}.
 *
 * @public
 */
export class Requesting {
	/** The requested path. */
	readonly path: Path.Valid;

	/**
	 * The current broadcast, or undefined while nothing provides the path.
	 * Announced requests without an epoch follow publisher restarts. Blind and epoch-pinned
	 * requests retain their accepted instance. Another route of the same epoch taking over keeps
	 * this handle: its tracks resume on that route from the first group each reader lacks.
	 * Closing this handle affects only this request.
	 */
	readonly active: Getter<broadcast.Consumer | undefined>;

	/**
	 * Whether nothing can serve this path, as opposed to not having served it yet.
	 *
	 * True when the origin routes nothing here and nothing is prepared to answer: no session
	 * attached and no connection reconnecting toward one. False whenever {@link active} is
	 * set, and false while a connection is still coming up, so the ordinary page-load window
	 * before the first handshake reads as pending rather than as a missing broadcast.
	 * An announced request can still resolve on a later announcement. True once refused or closed.
	 */
	readonly unroutable: Getter<boolean>;

	/**
	 * Settles on refusal, origin shutdown, or explicit close.
	 * A pinned or blind request also ends when its accepted instance stops serving;
	 * an unpinned announced request waits for a later start instead.
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
	#scope: Scope;

	private constructor(state: OriginState, scope: Scope) {
		this.#state = state;
		this.#scope = scope;
		// True only when every attached session announces. One session that cannot means the
		// table is an incomplete picture, so a consumer gated on it has to keep its blind
		// fallback: the paths only that session carries never reach the table at all.
		this.#discovery = new Derived([state.sessions], ({ total, discovery }) =>
			total === 0 ? undefined : discovery === total,
		);
		registerWire(this, {
			routes: (path) => this.#routes(scope.path(path)),
			broadcasts:
				scope === Scope.all ? state.local : new Derived([state.local], (local) => scope.projectPaths(local)),
			advertised:
				scope === Scope.all
					? state.originated
					: new Derived([state.originated], (routes) => scope.projectRoutes(routes)),
			local: (path, epoch) => this.#local(scope.path(path), epoch),
			demand: (path, epoch, signal) => this.#demand(scope.path(path), epoch, signal),
		});
	}

	static {
		makeConsumer = (state, scope) => new Consumer(state, scope);
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
	 * Resolve a path, following publisher restarts when `announced` is true and no epoch is pinned.
	 * Announced requests clear `active` on an end and resolve again on the next start. A refusal
	 * is terminal. An epoch-pinned request never changes instance; a plain request keeps its
	 * accepted instance and may resolve blindly when discovery has no answer. Either one,
	 * once resolved through a discovered route, watches its path until closed.
	 * Announcement gating falls back to blind resolution when discovery is unavailable.
	 * Close the request to release its handle and announcement watch.
	 */
	request(path: Path.Valid, options: RequestOptions = {}): Requesting {
		if (options.announced && options.epoch === undefined) return this.#following(path, options);
		return this.#resolve(path, options);
	}

	#following(path: Path.Valid, options: RequestOptions): Requesting {
		const absolute = this.#scope.path(path);
		Path.Pattern.literal(path);
		let watch = this.#state.watchers.get(absolute);
		if (!watch) {
			watch = { users: 0, version: new Signal(0) };
			this.#state.watchers.set(absolute, watch);
		}
		watch.users++;
		const watched = watch;
		let unwatch: Dispose = () => {};
		let previous = this.#state.instance(absolute);
		const active = new Signal<broadcast.Consumer | undefined>(undefined);
		const closed = new Once<Error | null>();
		const missing = new Derived(
			[active, this.#state.answerers, closed],
			(value, answerers, error) => error !== undefined || (!value && answerers === 0),
		);
		let current: Requesting | undefined;
		// Whether `current` ever resolved, so losing it reads as an end rather than a refusal.
		let resolved = false;
		let subscriptions: Dispose[] = [];
		let disposed = false;
		const release = () => {
			for (const stop of subscriptions) stop();
			subscriptions = [];
			current?.close();
			current = undefined;
		};
		const finish = (error: Error | null) => {
			if (disposed) return;
			disposed = true;
			unwatch();
			if (--watched.users === 0) this.#state.watchers.delete(absolute);
			release();
			active.set(undefined);
			closed.set(error);
		};
		const ended = (error: Error | null | undefined) => {
			if (!error) return;
			// Losing an accepted instance waits for its next announcement; a refusal is final.
			if (resolved && error instanceof StreamError && error.code === StreamCode.Unroutable) {
				active.set(undefined);
				return;
			}
			finish(error);
		};
		const replace = (next?: Requesting) => {
			if (disposed) {
				next?.close();
				return;
			}
			const value = next?.active.peek();
			if (
				next &&
				current?.closed.peek() === undefined &&
				value &&
				value.closed === current?.active.peek()?.closed &&
				value.epoch === current.active.peek()?.epoch
			) {
				next.close();
				return;
			}
			release();
			current = next;
			active.set(value);
			if (!next) return;
			resolved = value !== undefined;
			subscriptions.push(
				next.active.subscribe((value) => {
					if (value) resolved = true;
					active.set(value);
				}),
			);
			subscriptions.push(next.closed.subscribe(ended));
			ended(next.closed.peek());
		};
		try {
			replace(this.#resolve(path, options));
		} catch (error) {
			finish(error instanceof Error ? error : new Error(String(error)));
			throw error;
		}
		const changed = () => {
			const shutdown = this.#state.closed.peek();
			if (shutdown !== undefined) {
				finish(shutdown);
				return;
			}
			// A refusal landing in the same turn as this wakeup is final; never retry past it.
			ended(current?.closed.peek());
			if (disposed) return;
			const next = this.#state.instance(absolute);
			const had = previous !== undefined;
			const same = previous && next && sameInstance(previous, next);
			previous = next;
			if (!next) {
				if (had) replace(this.#resolve(path, options));
			} else if (!same || !current || current.closed.peek() !== undefined) replace(this.#resolve(path, options));
		};
		if (!disposed) {
			unwatch = watched.version.subscribe(changed);
			changed();
		}

		return makeRequesting(path, active, missing, closed, () => finish(null));
	}

	#resolve(path: Path.Valid, options: RequestOptions): Requesting {
		const epoch = options.epoch;
		const relative = path;
		path = this.#scope.path(path);
		const requests = this.#state.requests.peek();
		if (!requests) {
			// Closed origin: a request that can never resolve, and says so.
			const closed = new Once<Error | null>();
			return makeRequesting(
				relative,
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
			const created: RequestSlot = {
				blind: 0,
				announced: 0,
				handles: new Set(),
				route: new Signal(this.#state.route(path, {})),
			};
			slot = created;
			this.#state.requests.mutate((map) => {
				map?.set(path, created);
			});
		}
		const closed = new Once<Error | null>();
		slot.handles.add(closed);
		// Counted, not subscribed: serving sessions decide blindness from the live discovery state.
		const announced = options.announced === true;
		if (announced) slot.announced += 1;
		else slot.blind += 1;
		// An announced request is a watch: sessions ask their peer about this path while it lives.
		let unwatch = announced ? this.#state.watch([path]) : undefined;
		this.#state.requests.mutate(() => {});

		// Hand out a handle of the request's own rather than the table's. Closing a consumer
		// closes the broadcast once it was the last one, and the table often holds the only
		// other handle, so lending its front out means an ordinary close() by one requester
		// can unpublish the path for everybody else.
		const taken = slot;

		// Memoized on the route's identity: the same front resolving again returns the handle
		// we already made, and only a real swap clones a new one (cloning before closing the
		// old, so a broadcast that both routes share never briefly loses its last handle).
		// Under an epoch the handle is a logical front instead: another route of the same epoch
		// serves the same broadcast, so the front re-sources its tracks and the handle stays.
		let released = false;
		let source: Resolution | undefined;
		let handle: broadcast.Consumer | undefined;
		let logical: Front | undefined;
		const own = (resolution: Resolution | undefined): broadcast.Consumer | undefined => {
			if (released) return undefined;
			const front = epoch === undefined || resolution?.epoch === epoch ? resolution?.front : undefined;
			if (logical && source?.epoch !== undefined && resolution?.epoch === source.epoch) {
				// Between routes of the instance, readers wait for the next one to answer.
				if (front) logical.serve(front);
				else logical.park();
				return handle;
			}
			if (front !== source?.front || resolution?.epoch !== source?.epoch) {
				const previous = handle;
				const concluded = logical;
				source = front ? { front, epoch: resolution?.epoch } : undefined;
				logical = front && resolution?.epoch !== undefined ? new Front() : undefined;
				logical?.serve(front as broadcast.Consumer);
				handle = logical ? logical.consumer : front?.clone();
				if (handle) {
					hooks.stampPath(handle, relative);
					hooks.stampEpoch(handle, resolution?.epoch);
				}
				concluded?.conclude();
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
		// Any other request watches its path once it resolves through a discovered route, so that
		// route stays discovered after whatever watch found it, such as a listing, is gone.
		const discovered = (resolution: Resolution | undefined) => {
			if (unwatch || !resolution?.front) return;
			// Only a route a session received; a local broadcast or dynamic route needs no discovery.
			const entry = this.#state.materialized.get(path)?.entry;
			if (!entry || entry.identity !== resolution.source || !received(entry)) return;
			unwatch = this.#state.watch([path]);
		};
		discovered(route.peek());
		const unsubscribe = route.subscribe((resolution) => {
			own(resolution);
			discovered(resolution);
		});

		// Only meaningful while nothing is routed, so it reads the route rather than `active`:
		// the two cannot disagree, since a routed path always has an answerer-independent
		// answer.
		const unroutable = new Derived(
			[route, this.#state.answerers, closed],
			(resolution, answerers, ended) =>
				ended !== undefined ||
				(epoch !== undefined && resolution?.epoch !== epoch) ||
				(!own(resolution) && answerers === 0),
		);

		return makeRequesting(relative, active, unroutable, closed, () => {
			// Releases this request's handle; the route itself belongs to the table.
			released = true;
			unsubscribe();
			logical?.conclude();
			logical = undefined;
			handle?.close();
			handle = undefined;
			source = undefined;

			unwatch?.();
			taken.handles.delete(closed);
			if (closed.peek() === undefined) closed.set(null);
			if (announced) taken.announced -= 1;
			else taken.blind -= 1;
			this.#state.requests.mutate(() => {});
			if (taken.handles.size > 0) return;

			// Defer the teardown a microtask: an effect whose rerun was triggered by the
			// answer resolving closes its old request and takes a new one in the same tick,
			// and tearing down in between would drop the answer it is about to read.
			queueMicrotask(() => {
				if (taken.handles.size > 0) return;
				// A refused slot already tore itself down, and the path may hold a newer one. A
				// detached slot owns the front it took.
				if (this.#state.requests.peek()?.get(path) !== taken) {
					this.#state.detach(path, taken);
					this.#state.forget(path);
					return;
				}
				this.#state.requests.mutate((map) => {
					map?.delete(path);
				});
				taken.answer?.close();
				taken.answer = undefined;
				taken.route.set(undefined);
				this.#state.releaseMaterialized(path);
				this.#state.forget(path);
			});
		});
	}

	/**
	 * The announced routes matching `scope`, as a live stream: every currently advertised
	 * route arrives first as `start`, then changes as they happen.
	 * Any pattern is accepted. A local broadcast appears once it announces, exactly as a
	 * peer sees it. A dynamic or received route announces the prefix it covers when its
	 * subtree overlaps the scope. The stream ends when the origin closes or the consumer is
	 * closed. Hidden routes are left out unless `options.hidden` opts in (see {@link announce.Options}).
	 * While open, it watches the scope's literal head: a `consume` session asks its peer for
	 * announcements there.
	 */
	announced(scope: Path.Pattern = Path.Pattern.all(), options?: announce.Options): announce.Consumer {
		const producer = new announce.Producer();
		const patterns = this.#scope.patterns(scope);
		void producer.closed.then(this.#state.watch([...patterns].map(scopeHead)));
		void this.#runAnnounced(producer, patterns, options?.hidden ?? false);
		return producer.consume();
	}

	/** The routes visible to one announcement watcher. */
	#listed(patterns: Path.Patterns, hidden: boolean): Map<Path.Valid, Presented> {
		const next = new Map<Path.Valid, Presented>();
		const covering = new CoveringRoot(this.#scope.root);
		const scopes = [...patterns]
			.sort((a, b) => Path.compareSpecificity(b.specificity(), a.specificity()))
			.map((pattern) => ({ pattern, head: scopeHead(pattern) }));
		for (const [path, candidates] of this.#state.snapshot()) {
			// The first candidate this reader can see wins, since the preferred one overall may not be.
			let entry: Candidate | undefined;
			let scope: (typeof scopes)[number] | undefined;
			for (const candidate of candidates) {
				scope = scopes.find(
					({ pattern, head }) =>
						(candidate.exact ? pattern.matches(path) : advertOverlaps(candidate, path, pattern)) &&
						(hidden || !hiddenBelow(head, path)),
				);
				if (scope) {
					entry = candidate;
					break;
				}
			}
			if (!entry || !scope) continue;
			const relative = covering.relative(path);
			if (relative === undefined) continue;
			next.set(relative, {
				identity: entry.identity,
				route: entry.route,
				captures: scopeCaptures(scope.pattern, path),
			});
		}
		return next;
	}

	async #runAnnounced(producer: announce.Producer, patterns: Path.Patterns, hidden: boolean): Promise<void> {
		// Keyed by the presented path (from the origin, not the scope), valued by identity
		// plus route. Diffing the instance rather than mere presence means a republish emits a
		// restart; a re-price of the same instance emits an update.
		let active = new Map<Path.Valid, Presented>();

		try {
			for (;;) {
				const local = this.#state.local.peek();
				const advertisedLocal = this.#state.advertisedLocal.peek();
				const routes = this.#state.routes.peek();
				if (local === undefined && advertisedLocal === undefined && routes === undefined) break;

				const next = this.#listed(patterns, hidden);

				for (const [path, snap] of active) {
					if (!next.has(path))
						producer.append({
							prefix: path,
							captures: snap.captures,
							kind: "end",
							route: snap.route,
						});
				}
				for (const [path, snap] of next) {
					const prev = active.get(path);
					if (!prev) {
						producer.append({ prefix: path, captures: snap.captures, kind: "start", route: snap.route });
					} else if (!sameInstance(prev, snap)) {
						producer.append({ prefix: path, captures: snap.captures, kind: "restart", route: snap.route });
					} else if (!routesEqual(prev.route, snap.route)) {
						producer.append({ prefix: path, captures: snap.captures, kind: "update", route: snap.route });
					}
				}
				active = next;

				// Synced once no session is still delivering an initial set this stream could
				// list: one under a hidden segment holds only a reader that opts in.
				let behind = false;
				for (const prefix of this.#state.snapshots.peek().keys()) {
					for (const pattern of patterns) {
						if (!pattern.overlaps(Path.Pattern.subtree(prefix))) continue;
						if (hidden || !hiddenBelow(scopeHead(pattern), prefix)) behind = true;
					}
				}
				if (behind) producer.behind();
				else producer.synced();

				await Signal.race(
					this.#state.local,
					this.#state.advertisedLocal,
					this.#state.routes,
					this.#state.snapshots,
					producer.closed,
				);

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
	#local(path: Path.Valid, epoch?: Epoch.Valid): broadcast.Consumer | undefined {
		const local = this.#state.local.peek()?.get(path);
		if (!local || !this.#state.localWins(path, this.#state.bestEntry(path, received))) return undefined;
		if (epoch !== undefined && this.#state.advertisedLocal.peek()?.get(path)?.epoch !== epoch) return undefined;
		return local;
	}

	/**
	 * Resolve `path` for serving: an announced local broadcast, or wait for an originated
	 * dynamic to accept it. Undefined when nothing here can serve the path.
	 *
	 * @internal
	 */
	async #demand(
		path: Path.Valid,
		epoch?: Epoch.Valid,
		signal?: AbortSignal,
	): Promise<broadcast.Consumer | undefined> {
		signal?.throwIfAborted();
		const cancelled = new Once<true>();
		let release: Dispose | undefined;
		const abort = () => {
			// Withdraw before a handler can answer in the same turn as cancellation.
			release?.();
			cancelled.set(true);
		};
		signal?.addEventListener("abort", abort, { once: true });
		try {
			// Each pass asks the instance that wins now; an epoch change releasing the request starts another.
			for (;;) {
				signal?.throwIfAborted();
				const local = this.#local(path);
				if (local) {
					// A peer naming one publisher instance is never handed another.
					if (epoch !== undefined && this.#state.advertisedLocal.peek()?.get(path)?.epoch !== epoch)
						throw unroutable();
					return local;
				}
				const entry = this.#state.bestEntry(path, received);
				if (!entry?.server) return undefined;
				if (epoch !== undefined && entry.route.peek().epoch !== epoch) throw unroutable();

				const server = entry.server;
				const live = server.served.get(path);
				if (live && live.closed.peek() === undefined) return live;

				const resets = server.resets;
				server.enqueue(path);
				server.demanding.set(path, (server.demanding.get(path) ?? 0) + 1);
				release = () => {
					// Abort and finally share this registration; release it only once.
					release = undefined;
					// A reset already cleared the count, and a later pass may have registered anew.
					if (server.resets === resets) {
						const n = (server.demanding.get(path) ?? 1) - 1;
						if (n <= 0) {
							server.demanding.delete(path);
							server.rejected.delete(path);
							// Only the final waiter changes which pending routes are still needed.
							this.#state.forget(path);
						} else server.demanding.set(path, n);
					}
				};
				try {
					for (;;) {
						signal?.throwIfAborted();
						// The route may name another instance by the time its handler answers.
						if (epoch !== undefined && this.#state.bestEntry(path, received)?.route.peek().epoch !== epoch)
							throw unroutable();
						const served = server.served.get(path);
						if (served && served.closed.peek() === undefined) return served;
						const rejected = server.rejected.get(path);
						if (rejected) {
							throw rejected;
						}
						const closed = server.closed.peek();
						if (closed !== undefined) {
							if (closed) throw closed;
							return undefined;
						}
						// Released by an epoch change, which also dropped this pass's count: ask again.
						if (server.resets !== resets) break;
						if (!server.pending.has(path)) return undefined;
						await race([Signal.race(server.settled, server.closed), cancelled]);
					}
				} finally {
					release?.();
				}
			}
		} finally {
			signal?.removeEventListener("abort", abort);
		}
	}
}

/**
 * A served route from {@link Producer.dynamic}: advertises a prefix and answers the
 * requests beneath it.
 *
 * Drop it (or {@link close}) to retract the route and reject anything still waiting
 * with {@link StreamCode.Unroutable}. {@link update} re-prices it in place.
 *
 * @public
 */
export class Dynamic {
	/** The prefix this handle advertises. */
	readonly prefix: Path.Valid;

	#entry: RouteEntry;
	#state: OriginState;
	#retract: Dispose;
	#publisher?: Producer;
	#closed = false;

	private constructor(
		prefix: Path.Valid,
		entry: RouteEntry,
		state: OriginState,
		retract: Dispose,
		publisher?: Producer,
	) {
		this.prefix = Path.stripPrefix(entry.scope.root, prefix) ?? Path.empty();
		this.#entry = entry;
		this.#state = state;
		this.#retract = retract;
		this.#publisher = publisher;
	}

	static {
		makeDynamic = (prefix, entry, state, retract, publisher) =>
			new Dynamic(prefix, entry, state, retract, publisher);
	}

	/** The route this handle advertises. */
	get route(): Route {
		return this.#entry.route.peek();
	}

	/**
	 * Replace the route in place. The prefix is fixed at announce time.
	 *
	 * The route is taken as given. At the same epoch this re-prices: consumers see an update
	 * and every handle survives. Another epoch, or none, names another publisher instance:
	 * consumers see a restart, resolved requests keep their old broadcast until closed, and a
	 * re-request never joins it. Requests still waiting on this handle carry over: the handler
	 * is asked again under the new epoch, and its answer to a request asked before the change
	 * is dropped, never served under the new epoch. A new or unresolved request pinned to the
	 * old epoch is refused as unroutable. The answers already served are forgotten, so the next
	 * request for one of those paths asks the handler again too. The broadcasts it served keep
	 * running for the subscriptions already on them; close them to end those too.
	 * Route selection still applies: another route still at the old epoch outranks one
	 * without. To re-price, start from the current route, `updateRoute({ ...dynamic.route, cost })`.
	 *
	 * @internal
	 */
	updateRoute(
		route: Route | { epoch?: Route["epoch"]; hops?: Route["hops"]; cost?: Route["cost"]; auth?: Route["auth"] },
	): void {
		if (this.#closed) throw new Error("dynamic is closed");
		const next = Route.normalize(route);
		const previous = this.#entry.route.peek().epoch;
		if (previous !== next.epoch) this.#state.restart(this.#entry);
		this.#entry.route.set(next);
		if (previous !== next.epoch) this.#entry.server?.reset();
		this.#state.rebuildOriginated();
		this.#state.refreshPrefix(Path.join(this.#entry.scope.root, this.prefix));
		this.#state.routes.mutate(() => {});
	}

	/** Retract the route and reject anything still waiting. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#retract();
	}

	/** Requests under this prefix, as they arrive, each to {@link Request.prepare} or reject. */
	async *requested(): AsyncIterableIterator<Request> {
		const server = this.#entry.server;
		if (!server) return;
		let current: Request | undefined;
		const drop = () => {
			current?.reject(unroutable());
			current = undefined;
		};
		// Keep the cursor while draining: restarting it would rescan deleted entries.
		let queued = server.queue.peek().values();
		try {
			for (;;) {
				const next = queued.next();
				if (!next.done) {
					drop();
					server.queue.mutate((queue) => {
						queue.delete(next.value);
					});
					current = next.value;
					current.publisher = this.#publisher;
					yield next.value;
					continue;
				}
				if (server.closed.peek() !== undefined) return;
				await Signal.race(server.queue, server.closed);
				queued = server.queue.peek().values();
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
	#withdrawn = false;
	/** @internal Set only by an application claim. */
	publisher?: Producer;

	private constructor(path: Path.Valid, server: ServeState) {
		this.path = path;
		this.#server = server;
	}

	static {
		makeRequest = (path, server) => new Request(path, server);
		finishRequest = (request, err) => {
			request.#done = true;
			request.#withdrawn = true;
			void err;
		};
	}

	/** Start answering as a new publisher instance, hidden until published. */
	prepare(epoch: Epoch.Valid): Prepared {
		if (this.#done) throw new Error("request is closed");
		if (!this.publisher) throw new Error("forwarded requests cannot be prepared");
		this.#done = true;
		const answer = this.publisher.createBroadcast(this.path);
		// Withheld until publish, so the answer cannot announce itself under another
		// epoch or skip the request's admission.
		const announcer = hooks.withholdAnnouncer(answer);
		if (!announcer) throw new Error("origin is closed");
		return new Prepared(this, answer, announcer, epoch);
	}

	/** @internal Publish a prepared answer after its tracks have been filled. */
	publishPrepared(
		answer: broadcast.Producer,
		announcer: broadcast.Announcer,
		epoch: Epoch.Valid,
	): broadcast.Producer {
		if (!this.#server.has(this)) throw new Error("request is closed");
		if (answer.closed.peek() !== undefined) throw new Error("broadcast is closed");
		hooks.attachAnnouncer(answer, announcer);
		// The exact route usually settles the request on its own, resolving its waiters
		// through the new epoch; answer it directly if it is still pending.
		answer.announce({ epoch });
		if (this.#server.has(this)) this.#server.accept(this, answer.consume());
		return answer;
	}

	/** @internal Refuse the request a prepared answer abandoned. */
	rejectPrepared(err: Error): void {
		this.#server.reject(this, err);
	}

	/**
	 * Resolve a session-forwarded request with its peer's routed broadcast.
	 *
	 * The caller keeps producing into `broadcast`; repeat requests for the path share
	 * it for as long as it stays live.
	 *
	 * A JS app does not proxy: a broadcast a session delivered would go out labeled with
	 * this origin's hop. To serve upstream content, copy its tracks into a prepared answer
	 * and publish it.
	 *
	 * @throws Error when handed a broadcast a session delivered. The request stays open.
	 * @internal
	 */
	accept(source: broadcast.Producer | broadcast.Consumer): void {
		if (this.#done) {
			// A withdrawn request takes no answer. Only the handed-off consumer is ours;
			// the application still owns its producer and any other consumers.
			if (this.#withdrawn && source instanceof broadcast.Consumer) source.close();
			return;
		}
		if (this.publisher) throw new Error("claim answers must be prepared and published");
		if (this.#server.originated && !(source instanceof broadcast.Producer) && wireOf(source).fromSession) {
			throw new Error("origin cannot serve a broadcast it did not produce");
		}
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

/** A claim answer being filled before its exact route is announced. */
export class Prepared {
	/** The unannounced broadcast to fill before publishing; only {@link publish} announces it. */
	readonly broadcast: broadcast.Producer;
	readonly #request: Request;
	readonly #announcer: broadcast.Announcer;
	readonly #epoch: Epoch.Valid;
	#done = false;

	/** @internal */
	constructor(request: Request, answer: broadcast.Producer, announcer: broadcast.Announcer, epoch: Epoch.Valid) {
		this.#request = request;
		this.broadcast = answer;
		this.#announcer = announcer;
		this.#epoch = epoch;
	}

	/** Announce the filled broadcast and resolve every waiting requester, refusing them if it cannot be announced. */
	publish(): broadcast.Producer {
		if (this.#done) throw new Error("prepared answer is closed");
		this.#done = true;
		try {
			return this.#request.publishPrepared(this.broadcast, this.#announcer, this.#epoch);
		} catch (err) {
			this.broadcast.close();
			this.#request.rejectPrepared(err instanceof Error ? err : new Error(String(err)));
			throw err;
		}
	}

	/** Abandon the answer unpublished, closing its broadcast and resolving every waiting requester with `err`. */
	reject(err: Error): void {
		if (this.#done) return;
		this.#done = true;
		this.broadcast.close();
		this.#request.rejectPrepared(err);
	}
}

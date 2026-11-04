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
import { Derived, type Dispose, type GetPromise, type Getter, Once, race, Signal } from "@moq/signals";
import type * as Announce from "./announce.ts";
import * as announce from "./announced.ts";
import type { Credential } from "./auth.ts";
import * as broadcast from "./broadcast.ts";
import { StreamCode, StreamError } from "./error.ts";
import { type Cost, RouteInfo, routesEqual } from "./hop.ts";
import * as Instance from "./instance.ts";
import {
	compareRouteCandidates,
	compareRoutes,
	coveringPrefixes,
	Epochs,
	firstEligible,
	hiddenBelow,
	hooks,
	scopeCaptures,
	scopeHead,
	scopeOverlaps,
	superseded,
} from "./internal.ts";
import * as Path from "./path.ts";
import { Front } from "./resume.ts";
import { closeUnwanted } from "./util/announce.ts";
import {
	type Advertised,
	type Advertisements,
	type InterestListener,
	registerWire,
	type Served,
	sameInstance,
	wireOf,
} from "./wire.ts";

export type { Cost, Hop, RouteInfo } from "./hop.ts";
export { HopSchema, isAnonymous } from "./hop.ts";

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
			// Each keeps its own prefix, so supersession never compares across the merged prefixes.
			covering.push([path, visible.map((value) => ({ ...value, prefix: path }))]);
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
 * next session is another publisher instance unless an instance says otherwise. A slot still
 * waiting is answered by the next session.
 *
 * `handles` holds the `closed` of each open {@link Requesting} on the path.
 *
 * A refusal is terminal, even while another source still serves: the slot ends, every handle
 * closes with the handler's error, and the slot leaves the table, so the next request asks
 * afresh. A refusal never falls through to a broader prefix or another advertiser.
 */
interface RequestSlot {
	/** Open plain handles that can use a peer without discovery. */
	blind: number;
	answer?: broadcast.Consumer;
	readonly handles: Set<Once<Error | null>>;
	/** Pending plain handles, each ending itself once synced discovery leaves it no route. */
	readonly absent: Set<() => void>;
	readonly route: Signal<Resolution | undefined>;
	/** The front a detached slot took from the table, closed with its last handle. */
	retired?: broadcast.Consumer;
	/** The route a detached slot asked to take over under the same instance, until it answers. */
	failover?: RouteEntry;
	/**
	 * The instance a pinned slot resolves through its own routes, as Rust's lookup does, while
	 * another instance wins the path. Such a slot is detached from the start: never in the
	 * table's `requests`, it is shared only by requests pinned to the same instance.
	 */
	readonly pin?: Instance.Valid;
}

/** One path resolution and the instance of the route that can currently serve it. */
interface Resolution {
	readonly front?: broadcast.Consumer;
	readonly instance?: Instance.Valid;
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
 */
interface RouteEntry {
	readonly identity: object;
	readonly scope: Scope;
	/** The paths the entry may serve beneath its prefix, when its producer is scoped. */
	readonly claim?: Path.Patterns;
	readonly route: Signal<RouteInfo>;
	readonly originated: boolean;
	readonly server?: ServeState;
}

/** One advertisement at a prefix. `exact` marks an announced local broadcast, which is only its own path. */
interface Candidate extends Advertised {
	readonly exact: boolean;
}

/**
 * Orders advertisements at a prefix: the better route, then a local broadcast on a tie, then
 * fewer hops. A remaining tie is left to the table's newest-first order; see
 * {@link compareRouteCandidates}.
 */
function compareCandidates(a: Candidate, b: Candidate): number {
	return (
		compareRoutes(a.route, b.route) ||
		Number(b.exact) - Number(a.exact) ||
		a.route.hops.length - b.route.hops.length
	);
}

/**
 * The preferred of `entries` (newest first, at `prefix`) for resolving `path`, not skipped
 * nor superseded: see {@link compareRouteCandidates}, then newest.
 */
function preferredEntry(
	path: Path.Valid,
	prefix: Path.Valid,
	entries: readonly RouteEntry[],
	skip?: (entry: RouteEntry) => boolean,
	others: readonly RouteInfo[] = [],
): RouteEntry | undefined {
	const eligible = entries.filter((entry) => !skip?.(entry));
	// `others` compete at the prefix too, such as a broadcast announced here at the path.
	const epochs = new Epochs([...eligible.map((entry) => entry.route.peek()), ...others]);
	let best: RouteEntry | undefined;
	for (const entry of eligible) {
		// Only the highest epoch of an instance is eligible, however cheap an older one is.
		if (epochs.supersedes(entry.route.peek())) continue;
		if (!best) {
			best = entry;
			continue;
		}
		const a = entry.route.peek();
		const b = best.route.peek();
		if (compareRouteCandidates(path, prefix, a, b) < 0) best = entry;
	}
	return best;
}

/** Whether a session received `entry`, so it is never forwarded to a peer. */
function received(entry: RouteEntry): boolean {
	return !entry.originated;
}

/** What a peer cannot be served through: a received route, or one of another instance than it pinned. */
function unservable(instance: Instance.Valid | undefined): (entry: RouteEntry) => boolean {
	if (instance === undefined) return received;
	return (entry) => received(entry) || entry.route.peek().instance !== instance;
}

function unroutable(): StreamError {
	return new StreamError(StreamCode.Unroutable, { message: "unroutable" });
}

// A selected handler refused these requests; a coalesced route gap can have the same error.
const refusedRequests = new WeakSet<GetPromise<Error | null>>();

/** A prefix route's queue of requests, from {@link Producer.routePrefix}: what a handler drains. */
class ServeState {
	readonly root: Path.Valid;
	/** The prefix the route claims; every served path is at or beneath it. */
	readonly prefix: Path.Valid;

	constructor(root: Path.Valid, prefix: Path.Valid) {
		this.root = root;
		this.prefix = prefix;
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

// A broadcast created through the origin: its producer, and the handle the origin routes.
interface Created {
	producer: broadcast.Producer;
	front: broadcast.Consumer;
}

// End a created broadcast the origin no longer routes. Consumer handles never end a produced
// broadcast, so the origin closes its producer; tracks already subscribed keep serving.
function retire({ producer, front }: Created): void {
	front.close();
	producer.close();
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
	created: Map<Path.Valid, Created> | undefined = new Map();
	local = new VersionedSignal<Map<Path.Valid, broadcast.Consumer> | undefined>(new Map());
	advertisedLocal = new VersionedSignal<Map<Path.Valid, RouteInfo> | undefined>(new Map());
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
	 * Readers select after filtering by their scope, then drop what a higher epoch of the
	 * same instance supersedes (see {@link firstEligible}), so a route they cannot see never
	 * hides one they can.
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
				});
			}
			if (list.length > 0) out.set(path, list);
		}
		const advertised = this.advertisedLocal.peek();
		for (const [path, front] of this.local.peek() ?? []) {
			const local = {
				identity: front,
				route: advertised?.get(path) ?? RouteInfo.default,
				exact: true,
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

	// Every open prefix route's server, advertised or not, so the origin's close ends each
	// handler loop even for a route that is hidden or withdrawn.
	servers = new Set<ServeState>();

	// Broadcasts materialized from a served route, keyed by exact path. Shared by every
	// request for the path so repeats reuse one accept; dropped (and closed) when the
	// providing route goes away or the last request releases it.
	materialized = new Map<
		Path.Valid,
		{ entry: RouteEntry; front: broadcast.Consumer; instance?: Instance.Valid; source: object }
	>();

	// Paths consumers asked for without waiting for an announcement; attached sessions
	// answer them with blind subscriptions. Never announced: an answered request is assumed
	// present, not known live, so it must not read as an availability claim.
	requests = new Signal<Map<Path.Valid, RequestSlot> | undefined>(new Map());

	// Path-local wakeups for followers, which survive the end of a resolved instance.
	// A counter rather than the instance: the instance is a plain object, so two routes with
	// identical metadata compare equal, and a coalesced write could hide a takeover
	// or the origin closing. Watchers read the table and `closed` themselves when woken.
	watchers = new Map<Path.Valid, { users: number; version: Signal<number> }>();

	// Slots taken out of `requests` because another instance won their path, still open for
	// their handles. Nothing joins them, but they are refreshed with the table, so one ends
	// once its own instance stops serving, except retained answers from an in-place Restart.
	detached = new Map<Path.Valid, Set<RequestSlot>>();

	// An in-place Restart forgets served answers for future requests, but resolved handles
	// retain their old broadcast without participating in routing or same-instance failover.
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
			this.settle(prefix);
		};
	}

	// The prefixes whose pending plain requests may have lost their last reason to wait.
	#unsettled = new Set<Path.Valid>();

	/**
	 * Recheck the pending plain requests under the absolute `prefix` once the current task
	 * settles, so a session releasing its initial set and downgrading reads as one step.
	 */
	settle(prefix: Path.Valid): void {
		if (this.#unsettled.size === 0) queueMicrotask(() => this.#settled());
		this.#unsettled.add(prefix);
	}

	#settled(): void {
		const requests = this.requests.peek();
		const visited = new Set<WatchNode>();
		for (const prefix of this.#unsettled) {
			// Every request watches its own path, so the watches beneath the prefix name exactly
			// the requests it can settle, without scanning the rest of the table.
			let node: WatchNode | undefined = this.#watches;
			for (const part of prefix === Path.empty() ? [] : prefix.split("/")) node = node?.children.get(part);
			const stack = node ? [node] : [];
			for (let next = stack.pop(); next; next = stack.pop()) {
				if (visited.has(next)) continue;
				visited.add(next);
				// Children first: settling a request may prune its node.
				stack.push(...next.children.values());
				const slot = requests?.get(next.path);
				if (slot) for (const absent of [...slot.absent]) absent();
				// A pinned slot waits detached.
				for (const other of [...(this.detached.get(next.path) ?? [])]) {
					for (const absent of [...other.absent]) absent();
				}
			}
		}
		this.#unsettled.clear();
	}

	/** Whether every attached session discovers and none owes an initial set covering `path`. */
	synced(path: Path.Valid): boolean {
		const { total, discovery } = this.sessions.peek();
		if (total === 0 || discovery < total) return false;
		const snapshots = this.snapshots.peek();
		for (const prefix of coveringPrefixes(path)) if (snapshots.has(prefix)) return false;
		return true;
	}

	// The literal heads readers are watching, counted, as a trie: announcement streams,
	// requests, and followers. Sessions ask their peer for announcements under the broadest of
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
		// Until it resolves, a pinned slot takes the best route of its instance; then it stays on
		// that instance, like any detached slot.
		if (slot.pin !== undefined && !(current?.front && current.source)) {
			this.pick(path, slot, slot.pin);
			return;
		}
		if (current?.front && current.source) {
			if (this.detached.get(path)?.has(slot)) {
				this.failover(path, slot, current.source, current.instance);
				return;
			}
			if (!this.serves(path, current.source, current.instance, false)) {
				this.drop(path, slot, unroutable());
				return;
			}
			const next = this.instance(path);
			const held = { identity: current.source, route: { instance: current.instance } };
			if (next && !sameInstance(held, next)) {
				this.retire(path, slot);
				return;
			}
		}
		slot.route.set(this.route(path, slot));
	}

	/**
	 * Keep a detached slot on its instance: on its own source while that serves, else on another
	 * route of the same instance, which serves the same bytes, keeping the old front until that one
	 * answers. Without such a route the slot ends.
	 */
	failover(path: Path.Valid, slot: RequestSlot, source: object, instance: Instance.Valid | undefined): void {
		if (this.serves(path, source, instance, true)) {
			// Its own source serves again: a takeover still pending speaks for nobody.
			slot.failover = undefined;
			return;
		}
		// A local broadcast announced under the instance serves the same bytes with no round trip.
		const local = this.local.peek()?.get(path);
		if (instance !== undefined && local && this.advertisedLocal.peek()?.get(path)?.instance === instance) {
			slot.failover = undefined;
			this.retain(path, slot, undefined);
			slot.route.set({ front: local, instance, source: local });
			return;
		}
		const entry =
			instance === undefined
				? undefined
				: this.bestEntry(
						path,
						(candidate) => !candidate.server || candidate.route.peek().instance !== instance,
					);
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
		this.retain(path, slot, served);
		slot.route.set({ front: served, instance, source: entry.identity });
	}

	/**
	 * Resolve a pinned slot that has not resolved yet through the best route of `instance` alone,
	 * as Rust's lookup does, whichever instance wins `path`. Without one it waits, and its handles
	 * end once discovery syncs.
	 */
	pick(path: Path.Valid, slot: RequestSlot, instance: Instance.Valid): void {
		const skip = (candidate: RouteEntry) => !candidate.server || candidate.route.peek().instance !== instance;
		const entry = this.bestEntry(path, skip);
		const local = this.local.peek()?.get(path);
		if (
			local &&
			this.advertisedLocal.peek()?.get(path)?.instance === instance &&
			this.localWins(path, entry, skip)
		) {
			slot.failover = undefined;
			this.retain(path, slot, undefined);
			slot.route.set({ front: local, instance, source: local });
			return;
		}
		if (!entry?.server) {
			slot.failover = undefined;
			slot.route.set(undefined);
			return;
		}
		const served = entry.server.served.get(path);
		if (!served || served.closed.peek() !== undefined) {
			// Held by the slot, so the pending request outlives a forget.
			slot.failover = entry;
			entry.server.enqueue(path);
			slot.route.set({ instance });
			return;
		}
		slot.failover = undefined;
		this.retain(path, slot, served);
		slot.route.set({ front: served, instance, source: entry.identity });
	}

	/**
	 * Make `front` the session answer `slot` holds, closing the one it held unless the cache or
	 * another detached slot shares it: a session serves one answer per path, so a pinned slot and
	 * the path's own requests can take the same one.
	 */
	retain(path: Path.Valid, slot: RequestSlot, front: broadcast.Consumer | undefined): void {
		const previous = slot.retired;
		slot.retired = front;
		if (previous && previous !== front) this.closeUnheld(path, previous);
	}

	/** Close a session answer nothing at `path` holds any more: neither the cache nor a detached slot. */
	closeUnheld(path: Path.Valid, front: broadcast.Consumer): void {
		if (this.materialized.get(path)?.front === front) return;
		for (const slot of this.detached.get(path) ?? []) if (slot.retired === front) return;
		front.close();
	}

	/**
	 * Whether the instance `source` served under `instance` still serves `path`. Under an
	 * instance any route of it does, unless `exact` asks for `source` itself; without one only
	 * `source` does, since nothing says another serves the same bytes.
	 */
	serves(path: Path.Valid, source: object, instance: Instance.Valid | undefined, exact: boolean): boolean {
		const matches = (candidate: object) => candidate === source || (!exact && instance !== undefined);
		const local = this.local.peek()?.get(path);
		const advertised = this.advertisedLocal.peek()?.get(path);
		if (local && advertised && advertised.instance === instance && matches(local)) return true;
		for (const [prefix, entries] of this.routes.peek() ?? []) {
			if (!Path.hasPrefix(prefix, path)) continue;
			for (const entry of entries) {
				if (!entry.server || !entry.scope.matches(path) || entry.route.peek().instance !== instance) continue;
				if (matches(entry.identity)) return true;
			}
		}
		return false;
	}

	/** The publisher instance a request for `path` resolves through, when one serves it. */
	instance(path: Path.Valid): Served | undefined {
		const entry = this.bestEntry(path);
		const local = this.local.peek()?.get(path);
		if (local && this.localWins(path, entry)) {
			return { identity: local, route: { instance: this.advertisedLocal.peek()?.get(path)?.instance } };
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

	/** Preserve this route's resolved answers before its server resets for another instance. */
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

	/** Whether a plain request needs the non-discovery session's blind answer. */
	blind(slot: RequestSlot): boolean {
		if (slot.blind === 0) return false;
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
			if (other.failover === entry) {
				for (const closed of other.handles) refusedRequests.add(closed);
				this.drop(path, other, err);
			}
		}
		const slot = this.requests.peek()?.get(path);
		if (!slot) return;
		// Only the route the request is waiting on speaks for it; one superseded by another
		// route or a local broadcast has a moot answer.
		if (this.bestEntry(path) !== entry || this.localWins(path, entry)) return;
		for (const closed of slot.handles) refusedRequests.add(closed);
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
		this.retain(path, slot, undefined);
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
		this.closeUnheld(path, cached.front);
	}

	/** The preferred entry on the most specific route covering `path`, ignoring skipped entries, if any. */
	bestEntry(path: Path.Valid, skip?: (entry: RouteEntry) => boolean): RouteEntry | undefined {
		const routes = this.routes.peek();
		if (!routes) return undefined;
		const local = this.local.peek()?.has(path) ? this.advertisedLocal.peek()?.get(path) : undefined;
		for (const prefix of coveringPrefixes(path)) {
			const entries = routes.get(prefix);
			if (!entries) continue;
			const entry = preferredEntry(
				path,
				prefix,
				entries,
				(candidate) => !candidate.scope.matches(path) || (skip?.(candidate) ?? false),
				prefix === path && local ? [local] : [],
			);
			if (entry) return entry;
		}
		return undefined;
	}

	/**
	 * Whether the announced local broadcast at `path` wins over `entry`, the best route a
	 * session or dynamic handle announced there. They compare as any two routes do: an
	 * identified route strictly cheaper than the local one wins, and the local broadcast wins a
	 * tie. A route at a shorter prefix never competes, since the most specific prefix wins
	 * outright. A higher epoch of the local broadcast's instance at the path supersedes it, among
	 * the routes `skip` leaves, which must be what `entry` was chosen from. False when nothing is
	 * announced locally at `path`.
	 */
	localWins(path: Path.Valid, entry: RouteEntry | undefined, skip?: (entry: RouteEntry) => boolean): boolean {
		const local = this.advertisedLocal.peek()?.get(path);
		if (!local || !this.local.peek()?.has(path)) return false;
		if (!entry || !this.routes.peek()?.get(path)?.includes(entry)) return true;
		const remote = entry.route.peek();
		// Within one instance only the higher epoch is eligible, however cheap the other is, so
		// any route at the path the request could take supersedes the local broadcast, not only
		// the best one.
		const rivals = (this.routes.peek()?.get(path) ?? [])
			.filter((candidate) => candidate.scope.matches(path) && !(skip?.(candidate) ?? false))
			.map((candidate) => candidate.route.peek());
		if (superseded(local, rivals)) return false;
		if (superseded(remote, [local])) return true;
		return compareRoutes(local, remote) <= 0;
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
			return { front: local, instance: this.advertisedLocal.peek()?.get(path)?.instance, source: local };
		}

		let cached = this.materialized.get(path);
		if (cached && cached.front.closed.peek() !== undefined) {
			this.materialized.delete(path);
			cached = undefined;
		}
		const instance = entry?.route.peek().instance;
		if (cached && cached.entry === entry && cached.instance === instance) return cached;
		if (!entry?.server) {
			this.releaseMaterialized(path);
			return slot.answer ? { front: slot.answer } : undefined;
		}

		const served = entry.server.served.get(path);
		if (served && served.closed.peek() === undefined) {
			const resolution = { entry, front: served, instance, source: entry.identity };
			this.materialized.set(path, resolution);
			if (cached && cached.front !== served) this.closeUnheld(path, cached.front);
			return resolution;
		}

		entry.server.enqueue(path);
		// The same instance over another route: the old front serves until the new one answers.
		const kept = cached?.instance === instance ? cached : undefined;
		return { front: kept?.front, instance, source: kept?.source };
	}
}

/**
 * The read side of an origin, borrowed: resolve broadcasts by path and watch what is
 * available, without publishing into it or owning its lifecycle.
 *
 * Code that only reads an origin should accept this type. {@link Consumer} implements it
 * and {@link Table} extends it, so `producer.consume()`, a borrowed table, and a structural
 * equivalent all fit.
 *
 * @public
 */
export interface Reader {
	/** Settles once the origin closes; see {@link Consumer.closed}. */
	readonly closed: GetPromise<Error | null>;

	/** Whether every attached session announces into the table; see {@link Consumer.discovery}. */
	readonly discovery: Getter<boolean | undefined>;

	/** Resolve one publisher instance at `path`; see {@link Consumer.request}. */
	request(path: Path.Valid, options?: RequestOptions): Requesting;

	/** Follow `path` across publisher instances; see {@link Consumer.follow}. */
	follow(path: Path.Valid, options?: FollowOptions): Following;

	/** The available broadcasts under `scope`, as a live stream; see {@link Consumer.announced}. */
	announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer;
}

/**
 * A non-owning handle on an origin: publish into it and read it, without its lifecycle.
 *
 * What a shared connection lends out. {@link Producer} implements it, so code that is
 * handed an origin rather than owning one should accept this type: closing the origin
 * stays the owner's alone, and a borrower cannot express it. A borrower that only reads
 * should accept {@link Reader} instead, so `producer.consume()` fits too.
 *
 * @public
 */
export interface Table extends Reader {
	/** Create a route at exactly `path`, owning a new broadcast; see {@link Producer.route}. */
	route(path: Path.Valid, options?: RouteOptions): Route;

	/** Create a route over `prefix` that yields requests; see {@link Producer.routePrefix}. */
	routePrefix(prefix: Path.Valid, options?: RouteOptions): Route;
}

/**
 * How an application advertises a {@link Route}: its visibility at creation, its price, its
 * credential, and, rarely, a stable instance.
 *
 * The default announces at once, at cost 0, under the session's scope, serving a fresh random
 * instance minted when the route is created.
 */
export interface RouteOptions {
	/**
	 * Create the route hidden: it exists for nobody until {@link Route.announce}. For a
	 * publisher that fills its catalog first, or answers a negotiation, before readers may find it.
	 */
	hidden?: boolean;
	/**
	 * What pulling content via this route costs: lower wins among routes covering the same
	 * prefix and anonymity. A publisher prices content it already produces at zero, and content
	 * it would have to start producing on demand higher.
	 */
	cost?: Cost;
	/**
	 * The credential the advertising request carries, from `auth.add()`. Only a session built
	 * on the handle that issued it presents it, as the token on its moq-transport
	 * PUBLISH_NAMESPACE; every other session advertises the route under its own scope.
	 */
	auth?: Credential;
	/**
	 * Serve this stable instance in place of a freshly minted one.
	 *
	 * Only for a publisher that continues the instance's sequence space across restarts,
	 * announcing a higher {@link epoch} each time, such as a counter persisted on disk. Readers
	 * stitch every route of one instance together, so a publisher that restarts its group
	 * sequences under an old instance splices unrelated groups into their tracks. The highest
	 * epoch wins over every older one of the instance, cheaper or not. Naming an instance grants
	 * nothing: publishing still needs the path's grant.
	 *
	 * Do not start a named instance with `~`: that marks one a relay minted (see
	 * {@link Instance.isMinted}), which signing refuses to bind and a catalog's instance check skips.
	 */
	instance?: Instance.Valid;
	/** Which incarnation of {@link instance} this is; raise it on each restart. Ignored without an instance. */
	epoch?: bigint;
}

/** Options for resolving one publisher instance with {@link Consumer.request}. */
export interface RequestOptions {
	/** Refuse a different publisher instance, including an asynchronous answer or a retry. */
	instance?: Instance.Valid;
}

/** Options for following a broadcast path with {@link Consumer.follow}. */
export interface FollowOptions {
	/** Follow only this publisher instance, waiting for it to return across gaps. */
	instance?: Instance.Valid;
}

// Route constructors, assigned in the classes' static blocks so neither class exposes one.
let makeRoute: (control: RouteControl) => Route;
let exactRoute: (producer: Producer, path: Path.Valid, options: RouteOptions, request: Request) => Route;

/**
 * The write side of an origin: route broadcasts into it by path and advertise them.
 *
 * Independent of any connection. A connection given this origin (via its `publish` option)
 * announces and serves the table's originated advertisements for as long as the session
 * lasts; the broadcasts themselves live until their producer closes or {@link close} tears
 * the origin down. A reconnecting session re-announces the table on each attach, so
 * advertisements made while offline surface on the next connection.
 *
 * Create a {@link route} at a path and fill its broadcast; create it hidden and
 * {@link Route.announce} it once its first tracks exist, since nobody can see or reach a hidden
 * route. To publish broadcasts on demand instead, claim a prefix with {@link routePrefix} and
 * answer the requests beneath it.
 *
 * @public
 */
export class Producer implements Table {
	#state = new OriginState();
	#scope = Scope.all;
	#requests?: Getter<ReadonlyMap<Path.Valid, RequestSlot> | undefined>;

	// Set on a derived handle, which borrows the origin: what it registered, so its close
	// releases only that. The owner has none, since its close ends the whole origin.
	#borrowed?: { releases: Set<Dispose>; closed: boolean };

	// The reader backing the passthroughs, so holding a Producer never requires the
	// consume().x() stutter for everyday reads. One instance, so `discovery` keeps its
	// identity across reads.
	#reader = makeConsumer(this.#state, this.#scope);

	static {
		exactRoute = (producer, path, options, request) => producer.#exact(path, options, request);
	}

	constructor() {
		const thisProducer = this;
		registerWire(this, {
			receive: (prefix, route) => this.#receive(prefix, route),
			interests: (listener) => {
				if (this.#borrowed?.closed) return () => {};
				// What the listener currently wants, so a derived handle's close can retract it
				// and its session stops asking the peer for announcements nothing will land.
				const wants = new Map<Path.Valid, number>();
				const unlisten = this.#state.interests((absolute, wanted) => {
					for (const prefix of this.#scope.interests(absolute)) {
						const count = (wants.get(prefix) ?? 0) + (wanted ? 1 : -1);
						if (count > 0) wants.set(prefix, count);
						else wants.delete(prefix);
						listener(prefix, wanted);
					}
				});
				return this.#hold(() => {
					unlisten();
					for (const [prefix, count] of wants) for (let i = 0; i < count; i++) listener(prefix, false);
				});
			},
			accepts: (prefix) => this.#scope.overlaps(Path.join(this.#scope.root, prefix)),
			attach: (discovery) => this.#attach(discovery),
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
			blind: (slot) => this.#state.blind(slot as RequestSlot),
			answer: (path, front) => this.#answer(this.#scope.path(path), front),
			routes: (path) => wireOf(this.#reader).routes(path),
		});
	}

	/**
	 * Narrow this handle to patterns beneath root, presenting paths relative to that root.
	 * The result borrows the origin; see {@link close}.
	 */
	scope(root: Path.Valid, patterns: Path.Patterns): Producer {
		if (this.#borrowed?.closed) throw new Error("origin handle is closed");
		const scope = this.#scope.narrow(root, patterns);
		const producer = new Producer();
		producer.#state = this.#state;
		producer.#scope = scope;
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
	 * Create a route at exactly `path`, owning a new broadcast to fill.
	 *
	 * This is how local content enters an origin. Fill the broadcast through
	 * {@link Route.broadcast}. The route serves a fresh random instance, minted here and kept
	 * for its lifetime, across {@link Route.withdraw} and {@link Route.announce}, since its
	 * sequences continue: a new route is a new stream that never splices into an old one. A
	 * publisher that continues one sequence space across restarts may name a stable instance
	 * instead ({@link RouteOptions.instance}).
	 *
	 * Announced at once unless created {@link RouteOptions.hidden | hidden}. A hidden route
	 * exists for nobody until {@link Route.announce}: announce streams skip it and a request
	 * for its path finds nothing, on this origin exactly as at a peer. Create it hidden when
	 * the tracks a subscriber needs first, such as a catalog, are not there yet. To serve paths
	 * on demand without publishing each one, use {@link routePrefix}.
	 *
	 * Closing the route retracts it and closes its broadcast, and so does closing the
	 * broadcast. Creating a path again supersedes the previous route: the origin retracts it
	 * and closes its broadcast, and tracks already subscribed keep serving until they end. At
	 * the same instance, a local broadcast competes on cost and wins ties.
	 */
	route(path: Path.Valid, options: RouteOptions = {}): Route {
		return this.#exact(path, options);
	}

	/** An exact route at `path`, answering `request` on its first announce when it is a job's. */
	#exact(path: Path.Valid, options: RouteOptions, request?: Request): Route {
		path = this.#scope.path(path);
		if (this.#borrowed?.closed) throw new Error("origin handle is closed");
		publishPath(path);
		const created = this.#state.created;
		if (!created) throw new Error("origin is closed");
		// Normalized first, so a bad cost throws before anything is created.
		let info = RouteInfo.normalize({
			instance: options.instance ?? Instance.random(),
			epoch: options.instance === undefined ? 0n : options.epoch,
			cost: options.cost,
			auth: options.auth,
		});

		const producer = new broadcast.Producer();
		hooks.stampPath(producer, path);
		const front = producer.consume();
		const entry = { producer, front };
		const previous = created.get(path);
		created.set(path, entry);
		if (previous) {
			this.#retractExact(path, previous.front);
			retire(previous);
		}

		// The request a job's route answers when it first announces.
		let pending = request;
		// Drop the route when its broadcast closes, unless a recreate already replaced it: a
		// stale broadcast closing must not unpublish the live one. Clearing `created` also stops
		// a later announce. A job's unanswered request can never be answered now.
		const release = this.#hold(() => {
			stop();
			this.#retractExact(path, front);
			if (created.get(path) === entry) created.delete(path);
			retire(entry);
			const job = pending;
			pending = undefined;
			if (job) rejectJob(job, unroutable());
		});
		// Synchronous, so a request made right after the broadcast closes finds it gone.
		const stop = hooks.onClosed(producer, release);

		const announced = () => this.#state.local.peek()?.get(path) === front;
		const announce = () => {
			if (producer.closed.peek() !== undefined) throw new Error("broadcast is closed");
			const job = pending;
			if (!job) {
				this.#advertiseExact(path, front, info);
				return;
			}
			pending = undefined;
			try {
				// A request released meanwhile can never be answered, so this route never announces.
				if (!jobPending(job)) throw unroutable();
				this.#advertiseExact(path, front, info);
			} catch (err) {
				release();
				rejectJob(job, err instanceof Error ? err : new Error(String(err)));
				throw err;
			}
			answerJob(job);
		};
		const change = (next: RouteInfo) => {
			if (producer.closed.peek() !== undefined) throw new Error("broadcast is closed");
			if (!this.#state.created) throw new Error("origin is closed");
			info = next;
			if (announced()) this.#advertiseExact(path, front, info);
		};

		const route = makeRoute({
			broadcast: producer,
			instance: () => info.instance as Instance.Valid,
			announce,
			withdraw: () => this.#retractExact(path, front),
			announced,
			change: (update) => change(RouteInfo.normalize({ ...info, ...update })),
			requested: async function* () {},
			close: release,
			info: () => (announced() ? this.#state.advertisedLocal.peek()?.get(path) : undefined),
			update: (next) => {
				const named = RouteInfo.normalize(next);
				info = named.instance === undefined ? { ...named, instance: info.instance, epoch: info.epoch } : named;
				announce();
			},
		});
		if (!options.hidden) announce();
		return route;
	}

	#advertiseExact(path: Path.Valid, front: broadcast.Consumer, route: RouteInfo): void {
		if (!this.#state.local.peek()) throw new Error("origin is closed");
		if (this.#state.created?.get(path)?.front !== front) throw new Error("broadcast is closed");
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
	 * Create a route over `prefix` that yields the requests beneath it.
	 *
	 * A route is always a prefix: it claims `prefix` and every path beneath it (the empty
	 * prefix claims every path). A service that only serves some of them advertises the
	 * covering prefix and rejects the rest as they are requested; consumers narrow with a
	 * {@link Path.Pattern} locally. The route is priced, credentialed and shown like an exact
	 * one, and forwarded by sessions while it is visible.
	 *
	 * A consumer resolving a path under it that no announced local broadcast wins is handed to
	 * {@link Route.requested} as a {@link Request}: answer it by serving a broadcast already held
	 * ({@link Request.serve}), under this route's instance, or by creating the path's own exact
	 * route ({@link Request.route}), under a fresh instance, so restarting one job never splices
	 * into a copy of the last. A publisher that knows its broadcasts creates each one's exact
	 * {@link route} instead, so subscribers can enumerate them.
	 */
	routePrefix(prefix: Path.Valid, options: RouteOptions = {}): Route {
		const info = RouteInfo.normalize({
			instance: options.instance,
			epoch: options.epoch,
			cost: options.cost,
			auth: options.auth,
		});
		return this.#serving(prefix, info, true, true, !options.hidden);
	}

	/** Land a route a peer announced, taken as given. Not originated, so a session never announces it back. */
	#receive(prefix: Path.Valid, route?: Partial<RouteInfo>): Route {
		return this.#serving(prefix, RouteInfo.normalize(route), false, false, true);
	}

	#serving(prefix: Path.Valid, route: RouteInfo, originated: boolean, publishes: boolean, visible: boolean): Route {
		prefix = this.#scope.prefix(prefix);
		// A route that names no instance serves one of its own.
		if (route.instance === undefined) route = { ...route, instance: Instance.random(), epoch: 0n };
		publishPath(prefix);
		const server = new ServeState(this.#scope.root, prefix);
		server.onChange = (path) => this.#state.refresh(path);
		const entry: RouteEntry = {
			identity: {},
			scope: this.#scope,
			claim: this.#scope.allowed?.intersect(new Path.Patterns([Path.Pattern.subtree(prefix)])),
			route: new Signal(route),
			originated,
			server,
		};
		server.onReject = (path, err) => this.#state.refuse(path, entry, err);

		let inserted = false;
		let ended = false;
		const insert = () => {
			if (ended) throw new Error("route is closed");
			if (inserted) return;
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
			if (closed) throw new Error("origin is closed");
			inserted = true;
			this.#state.rebuildOriginated();
			this.#state.refreshPrefix(prefix);
		};
		// Take the entry out of the table, then end (or renew) the server before the requests
		// beneath it reroute, so none of them resolves through an answer it already gave.
		const remove = (server: () => void) => {
			if (inserted) {
				inserted = false;
				this.#state.routes.mutate((routes) => {
					const entries = routes?.get(prefix);
					if (!entries) return;
					const index = entries.indexOf(entry);
					if (index < 0) return;
					entries.splice(index, 1);
					if (entries.length === 0) routes?.delete(prefix);
				});
			}
			server();
			this.#state.rebuildOriginated();
			this.#state.refreshPrefix(prefix);
		};
		const release = this.#hold(() => {
			ended = true;
			this.#state.servers.delete(server);
			remove(() => server.close());
		});
		// A route made after its origin (or this handle) closed is born retracted.
		if (this.#borrowed?.closed || !this.#state.routes.peek()) {
			ended = true;
			server.close();
		} else {
			this.#state.servers.add(server);
			if (visible) insert();
		}

		const update = (next: RouteInfo) => {
			if (ended) throw new Error("route is closed");
			const previous = entry.route.peek().instance;
			const restart = inserted && previous !== next.instance;
			if (restart) this.#state.restart(entry);
			entry.route.set(next);
			if (previous !== next.instance) server.reset();
			if (!inserted) return;
			this.#state.rebuildOriginated();
			this.#state.refreshPrefix(prefix);
			this.#state.routes.mutate(() => {});
		};

		const publisher = publishes ? this : undefined;
		return makeRoute({
			instance: () => entry.route.peek().instance as Instance.Valid,
			announce: insert,
			withdraw: () => {
				if (ended || !inserted) return;
				remove(() => server.reset());
			},
			announced: () => inserted && this.#state.routes.peek() !== undefined,
			change: (changed) => update(RouteInfo.normalize({ ...entry.route.peek(), ...changed })),
			requested: () => serveRequests(server, publisher),
			close: release,
			info: () => (inserted ? entry.route.peek() : undefined),
			update: (next) => {
				// A route naming no instance keeps the one it serves.
				const named = RouteInfo.normalize(next);
				const current = entry.route.peek();
				update(
					named.instance === undefined
						? { ...named, instance: current.instance, epoch: current.epoch }
						: named,
				);
				insert();
			},
		});
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
		return this.#hold(() => this.#sessions(-1, discovery));
	}

	#sessions(delta: number, discovery: boolean): void {
		this.#state.sessions.update(({ total, discovery: d }) => ({
			total: total + delta,
			discovery: d + (discovery ? delta : 0),
		}));
		// Losing a session without discovery can leave every pending request synced.
		this.#state.settle(Path.empty());
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
				// publisher instance, since nothing (no instance) says it serves the same bytes.
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

	/** Resolve one publisher instance at `path`; see {@link Consumer.request}. */
	request(path: Path.Valid, options?: RequestOptions): Requesting {
		return this.#reader.request(path, options);
	}

	/** Follow `path` across publisher instances; see {@link Consumer.follow}. */
	follow(path: Path.Valid, options?: FollowOptions): Following {
		return this.#reader.follow(path, options);
	}

	/** The available broadcasts under `scope`, as a live stream; see {@link Consumer.announced}. */
	announced(scope?: Path.Pattern, options?: Announce.Options): Announce.Consumer {
		return this.#reader.announced(scope, options);
	}

	/**
	 * Close the origin, every broadcast it still routes, and its announcement streams. Idempotent.
	 *
	 * A handle from {@link scope} borrows the origin: closing it
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
		// Cleared before the broadcasts close, so each route's synchronous retraction finds
		// nothing left to rebuild rather than rebuilding the table once per route.
		const created = this.#state.created;
		this.#state.created = undefined;
		this.#state.local.update(() => undefined);
		this.#state.advertisedLocal.update(() => undefined);
		for (const entry of created?.values() ?? []) retire(entry);
		this.#state.routes.update((routes) => {
			for (const entries of routes?.values() ?? []) {
				for (const entry of entries) entry.server?.close(abort);
			}
			return undefined;
		});
		// A hidden or withdrawn prefix route is out of the table but its handler still waits.
		for (const server of this.#state.servers) server.close(abort);
		this.#state.servers.clear();
		this.#state.originated.update(() => undefined);
		// Materialized broadcasts are handles we opened; release them.
		for (const cached of this.#state.materialized.values()) {
			cached.front.close();
		}
		this.#state.materialized.clear();
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

// Same for Requesting and Following: a public constructor would let a caller forge a handle that no origin
// ever registered, whose lifecycle guarantees are then false. `@internal` alone would not
// stop it, since the declaration emit keeps the constructor.
let makeRequesting: (
	path: Path.Valid,
	active: Getter<broadcast.Consumer | undefined>,
	closed: Once<Error | null>,
	dispose: Dispose,
) => Requesting;

let makeFollowing: (
	path: Path.Valid,
	active: Getter<broadcast.Consumer | undefined>,
	closed: Once<Error | null>,
	dispose: Dispose,
) => Following;

let makeRequest: (path: Path.Valid, server: ServeState) => Request;
let finishRequest: (request: Request, err: Error) => void;
let setRequestPublisher: (request: Request, publisher: Producer | undefined) => void;
let jobPending: (request: Request) => boolean;
let answerJob: (request: Request) => void;
let rejectJob: (request: Request, err: Error) => void;

/**
 * An owned request for one publisher instance at a broadcast path; see {@link Consumer.request}.
 *
 * @public
 */
export class Requesting {
	/** The requested path. */
	readonly path: Path.Valid;

	/**
	 * The accepted broadcast, or undefined while the request is pending.
	 * Another route of the same instance taking over keeps
	 * this handle: its tracks resume on that route from the first group each reader lacks.
	 * Closing this handle affects only this request.
	 */
	readonly active: Getter<broadcast.Consumer | undefined>;

	/**
	 * Settles on refusal, origin shutdown, explicit close, or the accepted instance ending.
	 * Fails `Unroutable` while pending once discovery syncs without a route for it.
	 */
	readonly closed: GetPromise<Error | null>;

	#dispose: Dispose;
	#disposed = false;

	private constructor(
		path: Path.Valid,
		active: Getter<broadcast.Consumer | undefined>,
		closed: GetPromise<Error | null>,
		dispose: Dispose,
	) {
		this.path = path;
		this.active = active;
		this.closed = closed;
		this.#dispose = dispose;
	}

	static {
		makeRequesting = (path, active, closed, dispose) => new Requesting(path, active, closed, dispose);
	}

	/** Withdraw the request. The path stays routed for any other open request. Idempotent. */
	close(): void {
		if (this.#disposed) return;
		this.#disposed = true;
		this.#dispose();
	}
}

/**
 * An owned follower of the publisher instances announced at a broadcast path; see {@link Consumer.follow}.
 *
 * @public
 */
export class Following {
	/** The followed path. */
	readonly path: Path.Valid;

	/**
	 * The current announced broadcast, or undefined between publisher instances.
	 * Moves to each new instance, or waits for the pinned instance when one was requested.
	 * Another route of the same instance keeps this handle and resumes its tracks.
	 * Closing this handle affects only this follower.
	 */
	readonly active: Getter<broadcast.Consumer | undefined>;

	/**
	 * Settles on refusal, origin shutdown, or explicit close.
	 * Waits through gaps in announcements for a later start, even when pinned to an instance.
	 */
	readonly closed: GetPromise<Error | null>;

	#dispose: Dispose;
	#disposed = false;

	private constructor(
		path: Path.Valid,
		active: Getter<broadcast.Consumer | undefined>,
		closed: GetPromise<Error | null>,
		dispose: Dispose,
	) {
		this.path = path;
		this.active = active;
		this.closed = closed;
		this.#dispose = dispose;
	}

	static {
		makeFollowing = (path, active, closed, dispose) => new Following(path, active, closed, dispose);
	}

	/** Withdraw the follower. The path stays routed for any other open request. Idempotent. */
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
export class Consumer implements Reader {
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
			advertised:
				scope === Scope.all
					? state.originated
					: new Derived([state.originated], (routes) => scope.projectRoutes(routes)),
			local: (path, instance) => this.#local(scope.path(path), instance),
			demand: (path, instance, signal) => this.#demand(scope.path(path), instance, signal),
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
	 * Resolve one publisher instance serving `path`, pinned to `instance` when given.
	 *
	 * The request owns discovery of its path until closed and retries route churn while
	 * pending. It fails `Unroutable` once at least one session is attached and every
	 * session's discovery has synced without a route for it, and can use a session without
	 * discovery instead. With no session attached, such as between reconnects, it waits. Once resolved it keeps
	 * that instance, ending when the instance stops serving. Use {@link follow} to move on
	 * to the next one.
	 */
	request(path: Path.Valid, options: RequestOptions = {}): Requesting {
		return this.#resolve(path, options.instance, false);
	}

	/**
	 * Follow `path` across publisher instances, pinned to `instance` when given.
	 *
	 * `active` moves to each instance announced at the path, clears between them, and never
	 * closes on a gap: a pinned follower waits for its instance to return. Owns discovery of
	 * its path until closed. Requires discovery: a session without it never serves a follower.
	 * A selected handler's refusal or the origin closing ends it.
	 */
	follow(path: Path.Valid, options: FollowOptions = {}): Following {
		const instance = options.instance;
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
		let current: Requesting | undefined;
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
		const ended = (error: Error | null | undefined, request: Requesting | undefined = current) => {
			if (!error) return;
			// Provenance survives a coalesced end/start, even when both routes serve one instance.
			if (
				error instanceof StreamError &&
				error.code === StreamCode.Unroutable &&
				request &&
				!refusedRequests.has(request.closed)
			) {
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
				value.instance === current.active.peek()?.instance
			) {
				next.close();
				return;
			}
			release();
			current = next;
			active.set(value);
			if (!next) return;
			subscriptions.push(
				next.active.subscribe((value) => {
					active.set(value);
				}),
			);
			subscriptions.push(next.closed.subscribe((error) => ended(error, next)));
			ended(next.closed.peek(), next);
		};
		try {
			replace(this.#resolve(path, instance, true));
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
				if (had) replace(this.#resolve(path, instance, true));
			} else if (!same || !current || current.closed.peek() !== undefined)
				replace(this.#resolve(path, instance, true));
		};
		if (!disposed) {
			unwatch = watched.version.subscribe(changed);
			changed();
		}

		return makeFollowing(path, active, closed, () => finish(null));
	}

	// One resolution of `path`. A follower's (`announced`) takes only an announced front and
	// never reports absence: {@link follow} retries it on the next instance instead.
	#resolve(path: Path.Valid, instance: Instance.Valid | undefined, announced: boolean): Requesting {
		Path.Pattern.literal(path);
		const relative = path;
		path = this.#scope.path(path);
		const requests = this.#state.requests.peek();
		if (!requests) {
			// Closed origin: a request that can never resolve ends at once.
			const closed = new Once<Error | null>();
			closed.set(this.#state.closed.peek() ?? null);
			return makeRequesting(relative, new Signal<broadcast.Consumer | undefined>(undefined), closed, () => {});
		}

		// A pin naming another instance than the winner follows that instance's own routes, on a
		// slot shared only by requests pinned to it, rather than filter the winner afterward.
		const pin =
			instance !== undefined && this.#state.instance(path)?.route.instance !== instance ? instance : undefined;
		let slot =
			pin === undefined
				? requests.get(path)
				: [...(this.#state.detached.get(path) ?? [])].find(
						(other) => other.pin === pin && !this.#state.retained.has(other),
					);
		if (!slot && pin !== undefined) {
			const created: RequestSlot = {
				blind: 0,
				handles: new Set(),
				absent: new Set(),
				route: new Signal<Resolution | undefined>(undefined),
				pin,
			};
			slot = created;
			let detached = this.#state.detached.get(path);
			if (!detached) {
				detached = new Set();
				this.#state.detached.set(path, detached);
			}
			detached.add(created);
			this.#state.pick(path, created, pin);
		} else if (!slot) {
			// Seeded through the constructor, so a path the table already routes resolves on the
			// first read. It must not go through a silent set: that still captures the pre-seed
			// value as the baseline the next change is compared against, and never flushes to
			// clear it, so a seeded route retracting to undefined would look like no change and
			// notify nobody.
			const created: RequestSlot = {
				blind: 0,
				handles: new Set(),
				absent: new Set(),
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
		if (!announced) slot.blind += 1;
		// Resolution owns discovery from the first ask, not just after a route arrives.
		const unwatch = this.#state.watch([path]);
		this.#state.requests.mutate(() => {});

		// Hand out a handle of the request's own rather than the table's. Closing a consumer
		// closes the broadcast once it was the last one, and the table often holds the only
		// other handle, so lending its front out means an ordinary close() by one requester
		// can unpublish the path for everybody else.
		const taken = slot;

		// Memoized on the route's identity: the same front resolving again returns the handle
		// we already made, and only a real swap clones a new one (cloning before closing the
		// old, so a broadcast that both routes share never briefly loses its last handle).
		// Under a carried instance the handle is a logical front instead: another route of it
		// serves the same stream, so the front re-sources its tracks and the handle stays.
		let released = false;
		let source: Resolution | undefined;
		let handle: broadcast.Consumer | undefined;
		let logical: Front | undefined;
		const own = (resolution: Resolution | undefined): broadcast.Consumer | undefined => {
			if (released) return undefined;
			// A follower takes only a front something announced; a blind answer is for plain handles.
			const offered = announced && !resolution?.source ? undefined : resolution?.front;
			const front = instance === undefined || resolution?.instance === instance ? offered : undefined;
			if (logical && source?.instance !== undefined && resolution?.instance === source.instance) {
				// Between routes of the instance, readers wait for the next one to answer.
				if (front) logical.serve(front);
				else logical.park();
				return handle;
			}
			if (front !== source?.front || resolution?.instance !== source?.instance) {
				const previous = handle;
				const concluded = logical;
				source = front ? { front, instance: resolution?.instance } : undefined;
				// A minted instance names an announcement over a wire that carries none, whose
				// route may answer a re-request with another stream, so nothing re-sources it.
				const carried = resolution?.instance !== undefined && !Instance.isMinted(resolution.instance);
				logical = front && carried ? new Front() : undefined;
				logical?.serve(front as broadcast.Consumer);
				handle = logical ? logical.consumer : front?.clone();
				if (handle) {
					hooks.stampPath(handle, relative);
					hooks.stampInstance(handle, resolution?.instance);
					// A logical front lists what the route it was made through lists.
					if (front && hooks.unlisted(front)) hooks.stampUnlisted(handle);
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
		// A pending plain request reports absence, as Rust's request does, once discovery of
		// its path has synced without a route for its instance. Sessions hold that sync from
		// when the watch opens, so a fresh request waits for its peer's answer. A follower
		// keeps waiting for a start, and a resolved request ends only with its instance.
		const absent = () => {
			if (released || closed.peek() !== undefined) return;
			if (source) {
				taken.absent.delete(absent);
				return;
			}
			const resolution = route.peek();
			if (resolution && (instance === undefined || resolution.instance === instance)) return;
			if (!this.#state.synced(path)) return;
			closed.set(unroutable());
			release();
		};
		if (!announced) taken.absent.add(absent);

		const unsubscribe = route.subscribe((resolution) => {
			own(resolution);
			if (!announced) absent();
		});

		const release = () => {
			// Releases this request's handle; the route itself belongs to the table.
			if (released) return;
			released = true;
			unsubscribe();
			taken.absent.delete(absent);
			logical?.conclude();
			logical = undefined;
			handle?.close();
			handle = undefined;
			source = undefined;

			unwatch();
			taken.handles.delete(closed);
			if (closed.peek() === undefined) closed.set(null);
			if (!announced) taken.blind -= 1;
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
		};

		if (!announced) absent();
		return makeRequesting(relative, active, closed, release);
	}

	/**
	 * The announced routes matching `scope`, as a live stream: every currently advertised
	 * route arrives first as `start`, then changes as they happen.
	 * Any pattern is accepted. A local broadcast appears once it announces, exactly as a
	 * peer sees it. Only routes whose literal name matches the scope are listed, unless
	 * `options.covering` also asks for every dynamic or received route whose subtree
	 * overlaps it. The stream ends when the origin closes or the consumer is closed. Hidden
	 * routes are left out unless `options.hidden` opts in (see {@link Announce.Options}).
	 * While open, it watches the scope's literal head: a `consume` session asks its peer for
	 * announcements there.
	 */
	announced(scope: Path.Pattern = Path.Pattern.all(), options?: Announce.Options): Announce.Consumer {
		const producer = new announce.Producer();
		const patterns = this.#scope.patterns(scope);
		void producer.closed.then(this.#state.watch([...patterns].map(scopeHead)));
		void this.#runAnnounced(producer, patterns, options?.hidden ?? false, options?.covering ?? false);
		const consumer = producer.consume();
		// The last reader leaving ends the stream and its watch.
		closeUnwanted(producer);
		return consumer;
	}

	/** The routes visible to one announcement watcher. */
	#listed(patterns: Path.Patterns, hidden: boolean, covering: boolean): Map<Path.Valid, Presented> {
		const next = new Map<Path.Valid, Presented>();
		const rooted = new CoveringRoot(this.#scope.root);
		const scopes = [...patterns]
			.sort((a, b) => Path.compareSpecificity(b.specificity(), a.specificity()))
			.map((pattern) => ({ pattern, head: scopeHead(pattern) }));
		for (const [path, candidates] of this.#state.snapshot()) {
			// The first candidate this reader can see wins, since the preferred one overall may not be,
			// unless a higher epoch of its instance that the reader also sees supersedes it.
			const scopeOf = (candidate: Candidate) =>
				scopes.find(
					({ pattern, head }) =>
						(covering || pattern.matches(path)) &&
						(candidate.exact ? pattern.matches(path) : advertOverlaps(candidate, path, pattern)) &&
						(hidden || !hiddenBelow(head, path)),
				);
			const entry = firstEligible(candidates, (candidate) => scopeOf(candidate) !== undefined);
			const scope = entry && scopeOf(entry);
			if (!entry || !scope) continue;
			const relative = rooted.relative(path);
			if (relative === undefined) continue;
			next.set(relative, {
				identity: entry.identity,
				route: entry.route,
				captures: scopeCaptures(scope.pattern, path),
			});
		}
		return next;
	}

	async #runAnnounced(
		producer: announce.Producer,
		patterns: Path.Patterns,
		hidden: boolean,
		covering: boolean,
	): Promise<void> {
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

				const next = this.#listed(patterns, hidden, covering);

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
	 * The announced local broadcast at `path`, when it beats the originated routes there.
	 * Resolves through what rebuildOriginated advertised: a peer never sees received routes.
	 */
	#local(path: Path.Valid, instance?: Instance.Valid): broadcast.Consumer | undefined {
		const local = this.#state.local.peek()?.get(path);
		if (!local) return undefined;
		if (instance !== undefined && this.#state.advertisedLocal.peek()?.get(path)?.instance !== instance)
			return undefined;
		// Pinned, it competes only with its own instance's routes, whichever instance wins.
		const skip = unservable(instance);
		if (!this.#state.localWins(path, this.#state.bestEntry(path, skip), skip)) return undefined;
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
		instance?: Instance.Valid,
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
			// Each pass asks the instance that wins now; an instance change releasing the request starts another.
			for (;;) {
				signal?.throwIfAborted();
				// A peer naming one publisher instance takes that instance's own routes, whichever
				// wins, and is never handed another.
				const local = this.#local(path, instance);
				if (local) return local;
				const entry = this.#state.bestEntry(path, unservable(instance));
				if (!entry?.server) {
					if (instance !== undefined && (this.#local(path) || this.#state.bestEntry(path, received)?.server))
						throw unroutable();
					return undefined;
				}

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
						if (instance !== undefined && entry.route.peek().instance !== instance) throw unroutable();
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
						// Released by an instance change, which also dropped this pass's count: ask again.
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

/** What one {@link Route} does, exact or prefix. */
interface RouteControl {
	readonly broadcast?: broadcast.Producer;
	instance(): Instance.Valid;
	announce(): void;
	withdraw(): void;
	announced(): boolean;
	change(update: Partial<Pick<RouteInfo, "cost" | "auth">>): void;
	requested(): AsyncIterableIterator<Request>;
	close(): void;
	info(): RouteInfo | undefined;
	update(info: Partial<RouteInfo>): void;
}

/** The requests arriving on a prefix route's `server`, each answerable with a route of `publisher`'s when it has one. */
async function* serveRequests(server: ServeState, publisher: Producer | undefined): AsyncIterableIterator<Request> {
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
				setRequestPublisher(current, publisher);
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

/**
 * A route in an origin: an exact one owning its broadcast, or a prefix one answering the
 * requests beneath it.
 *
 * Create an exact route with {@link Producer.route}, a prefix route with
 * {@link Producer.routePrefix}, or a job's exact route with {@link Request.route}.
 *
 * ```ts
 * // Fill the catalog before readers can find it.
 * const route = origin.route(Path.from("live/alice"), { hidden: true });
 * route.broadcast.createTrack("catalog.json");
 * route.announce(); // visible now, same instance
 * route.setCost(5n); // re-prices in place
 * route.withdraw(); // hidden; announce() again continues the same stream
 * ```
 *
 * {@link close} retracts it and closes its broadcast, and so does closing the broadcast.
 *
 * @public
 */
export class Route {
	readonly #control: RouteControl;

	private constructor(control: RouteControl) {
		this.#control = control;
		registerWire(this, {
			get info() {
				return control.info();
			},
			update: (info) => control.update(info),
		});
	}

	static {
		makeRoute = (control) => new Route(control);
	}

	/** The broadcast this exact route owns, to fill with tracks. Throws on a prefix route, which owns none. */
	get broadcast(): broadcast.Producer {
		const producer = this.#control.broadcast;
		if (!producer) throw new Error("a prefix route owns no broadcast; answer its requests");
		return producer;
	}

	/** The stream this route serves, minted when it was created unless {@link RouteOptions.instance} named one. */
	get instance(): Instance.Valid {
		return this.#control.instance();
	}

	/**
	 * Make the route visible, or keep it so. Idempotent.
	 *
	 * Visible to local consumers before it returns. A job's route ({@link Request.route}) also
	 * answers the requesters waiting on its path. Throws once the route or its broadcast closed,
	 * and, announcing nothing, with {@link StreamCode | StreamCode.Unroutable} when a job's request
	 * was released or its prefix route retracted meanwhile: that route can never announce.
	 */
	announce(): void {
		this.#control.announce();
	}

	/**
	 * Hide the route from everyone, local and remote alike, keeping its broadcast and instance:
	 * {@link announce} brings the same stream back. Requests for the path fail with
	 * {@link StreamCode | StreamCode.Unroutable} meanwhile, while tracks already in flight carry
	 * on to their own end. Idempotent.
	 */
	withdraw(): void {
		this.#control.withdraw();
	}

	/** Whether the route is visible. */
	get announced(): boolean {
		return this.#control.announced();
	}

	/**
	 * Re-price the route: lower wins among routes covering the same prefix and anonymity. A
	 * visible route changes in place, as an update to its readers; a hidden one announces at
	 * this price. Throws once the route or its broadcast closed.
	 */
	setCost(cost: Cost): void {
		this.#control.change({ cost });
	}

	/**
	 * Swap the credential the advertising request carries; see {@link RouteOptions.auth}.
	 * Undefined advertises under the session's scope. Throws once the route or its broadcast closed.
	 */
	setAuth(auth?: Credential): void {
		this.#control.change({ auth });
	}

	/**
	 * The requests beneath this prefix route, as they arrive, each to answer with
	 * {@link Request.serve}, {@link Request.route} or {@link Request.reject}. Advancing the
	 * iterator or closing it without answering rejects the request. Ends once the route is
	 * closed or the origin closes, and at once on an exact route, which yields no requests.
	 */
	requested(): AsyncIterableIterator<Request> {
		return this.#control.requested();
	}

	/** Retract the route for good, closing its broadcast and rejecting anything still waiting. Idempotent. */
	close(): void {
		this.#control.close();
	}

	/** Calls {@link close}, so `using` retracts the route. */
	[Symbol.dispose](): void {
		this.close();
	}
}

/**
 * A pending request for a broadcast to be served on demand.
 *
 * Yielded by {@link Route.requested}. Answer it one of three ways: {@link serve} a broadcast
 * already held, under the prefix route's instance; create the path's own exact route with
 * {@link route}, under a fresh instance; or {@link reject} it with an error. Advancing the
 * iterator or closing it without answering rejects the request.
 *
 * @public
 */
export class Request {
	/** The path that was requested. */
	readonly path: Path.Valid;

	#server: ServeState;
	#done = false;
	#withdrawn = false;
	#publisher?: Producer;

	private constructor(path: Path.Valid, server: ServeState) {
		this.path = path;
		this.#server = server;
	}

	static {
		makeRequest = (path, server) => new Request(path, server);
		setRequestPublisher = (request, publisher) => {
			request.#publisher = publisher;
		};
		jobPending = (request) => request.#server.has(request);
		answerJob = (request) => {
			// Its exact route owns availability: never cache a published job under the
			// prefix, which would keep serving it after that exact route is withdrawn.
			request.#server.forget(Path.join(request.#server.root, request.path), request);
			request.#server.settled.update((n) => n + 1);
		};
		rejectJob = (request, err) => request.#server.reject(request, err);
		finishRequest = (request, err) => {
			request.#done = true;
			request.#withdrawn = true;
			void err;
		};
	}

	/**
	 * Answer with the path's own exact route, owning a new broadcast.
	 *
	 * The route serves a fresh instance unless `options` names one, so a job that ends and
	 * restarts beneath the same prefix is a new stream for its restarted sequences. Closing the
	 * route withdraws only this path: the prefix route keeps answering later requests. Created
	 * visible, the route announces now and answers the requesters waiting on its path. Created
	 * {@link RouteOptions.hidden | hidden}, they wait until {@link Route.announce}, so the job can
	 * fill its catalog first; closing the route or its broadcast before then refuses them.
	 *
	 * Throws {@link StreamCode | StreamCode.Unroutable} on a request a session forwarded, which can
	 * only {@link serve} its peer's broadcast, and, announcing nothing, once the request was
	 * released or its prefix route retracted. Otherwise it throws as {@link Producer.route} does.
	 */
	route(options: RouteOptions = {}): Route {
		if (this.#done) throw new Error("request is closed");
		if (!this.#publisher) throw unroutable();
		const route = exactRoute(this.#publisher, this.path, options, this);
		this.#done = true;
		return route;
	}

	/**
	 * Serve a held broadcast under this prefix route without announcing an exact route.
	 *
	 * Takes ownership of a consumer; a producer stays the caller's. Every answer at this
	 * path belongs to the prefix route's instance and must continue the same sequences and
	 * bytes. Start a new prefix route for different content, or use {@link route} for a job
	 * that can restart independently.
	 */
	serve(source: broadcast.Producer | broadcast.Consumer): void {
		if (this.#done) {
			// A withdrawn request takes no answer. Only the handed-off consumer is ours;
			// the application still owns its producer and any other consumers.
			if (this.#withdrawn && source instanceof broadcast.Consumer) source.close();
			return;
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

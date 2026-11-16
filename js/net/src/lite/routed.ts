// Lite-07: what the peer sends on its Announce Stream as a publisher, and the prefixes this
// session asks it for on its own.
//
// Every `routes()` reader of the session shares the one stream. The session asks the peer only
// for the covering set of what its readers want, no prefix at or below another, edited in place
// one ANNOUNCE_REQUEST at a time, and each reader is fed from the routes and claims the stream
// carries, presented under the reader's own prefix as the peer would present them to a request
// for it. Mirrors `subscriber/routed.rs` in rs/moq-net for a node that forwards nothing: an
// endpoint applies the peer's withdrawals and holds, and casts no doubt.

import { type Dispose, type GetPromise, race } from "@moq/signals";
import type * as announce from "../announced.ts";
import type { Drain } from "../connection/goaway.ts";
import type { BroadcastCache } from "../consume.ts";
import { error, ProtocolViolation } from "../error.ts";
import { Cost, type Hop, routesEqual, WireRoute } from "../hop.ts";
import { bestCandidate, coveringPrefixes, hiddenBelow, scopeCaptures, scopeOverlaps } from "../internal.ts";
import * as Path from "../path.ts";
import { SlotMap, type Slots } from "../util/limits.ts";
import {
	type Add,
	type AnnounceBroadcast,
	AnnounceHistory,
	type Refusal as AnnounceRefusal,
	type Base,
	type Dead,
	type Offer,
	type Refusable,
	ROUTE_MAX_HOPS,
	type RouteHop,
	requestFits,
	requestParts,
	type Unusable,
} from "./announce.ts";
import type { Carrier, Claim, Floors } from "./floors.ts";
import * as rekey from "./rekey.ts";
import type { Version } from "./version.ts";

/** What a `routes()` reader asked for. */
export interface Request {
	/** The prefix the reader's scope lies under. */
	prefix: Path.Valid;
	scope: Path.Pattern;
	/** Also report claims with a `.`-prefixed segment below the prefix. */
	hidden: boolean;
	/** Also report claims whose subtree overlaps the scope without matching it. */
	covering: boolean;
	/** Report each origin node's claim at a path rather than the best one. */
	perNode: boolean;
}

/** What the session's route layer is built on. */
export interface Session {
	quic: WebTransport;
	version: Version;
	/** This node's hop: a route whose path names it came back to its origin. */
	hop: Hop;
	floors: Floors;
	consumes: BroadcastCache;
	/** The session's announce cap: one slot per live route and claim. */
	slots: Slots;
	/** Rank a route last once the peer sent GOAWAY. */
	drained(route: WireRoute): WireRoute;
	goaway?: GetPromise<Drain>;
	/** What covers a path changed with no claim reporting it: every selection is checked anew. */
	revalidate(): void;
	/** Send one message on this session's Announce Stream. */
	send(msg: AnnounceBroadcast): Promise<void>;
}

/** One reader, and what it was last told of each path. */
interface Reader extends Request {
	announced: announce.Producer;
	/** What each path reports, by origin node with `perNode` and otherwise under `undefined`. */
	reported: Map<Path.Valid, Map<bigint | undefined, WireRoute>>;
	synced: boolean;
}

/** A prefix this session asked the peer for, by its Prefix ID. */
interface Asked {
	path: Path.Valid;
	hidden: boolean;
	/** The peer answered the ANNOUNCE_REQUEST adding it, so its stream may name it. */
	live: boolean;
	/** The peer sent its initial set. */
	synced: boolean;
}

/** One route the peer sent on the stream, by its Route ID. */
interface Route {
	id: bigint;
	node: bigint;
	/** The requested prefix it lies in. */
	region: Path.Valid;
	/** The prefix it covers, or undefined when this node refused the suffix the peer forwarded,
	 * which leaves the route refused for good. */
	prefix: Path.Valid | undefined;
	/** Its ROUTE_START said it lies at its own prefix, so a widening leaves it there. */
	exact: boolean;
	/** The path the peer last named, kept while this node refuses the rest, so a DEAD finds it. */
	path: RouteHop[];
	/** Undefined while this node refuses what the peer forwarded: no route at all, its claims
	 * offline, until an offer it can use. */
	offer: Offer | undefined;
	/** A DRAIN named a node on its path since its last offer: it ranks last. */
	drain: boolean;
	/** ROUTE_HOLD, or a DEAD that held it: no usable path to the publisher, until its next offer. */
	held: boolean;
	/** The Announce ID of each live claim, by path: one per path, checked by lookup. */
	claims: Map<Path.Valid, bigint>;
	/** The Announce IDs of its live claims whose path this node refused. */
	unnamed: Set<bigint>;
	/** The Announce IDs of its live claims at a path whose content this node refuses. */
	withheld: Set<bigint>;
	/** How the claims differ from the view's, applied whole at the next floor. */
	pending: Map<Path.Valid, Claim | undefined>;
	/** The prefixes whose claims an edit that widened the route took from the routes it merged,
	 * which the view keeps until the route's next floor says what covers each now. */
	stale: Set<Path.Valid>;
	/** The claims as of the last floor, and the revision it certified them at, if any. */
	view: { prefix: Path.Valid; hidden: boolean; revision: bigint | undefined; claims: Map<Path.Valid, Claim> };
	/** The view's claims no other route's newer floor withdrew or replaced, each as the newest
	 * floor certifying it has it, cost included. */
	shown: Map<Path.Valid, Claim>;
	carrier: Carrier;
	/** Who the route's claims are filed under in the consume cache, which keeps every route's
	 * claim so a consume ranks them for its own path. */
	owner: object;
}

type Joined = Route & { offer: Offer; prefix: Path.Valid };

/** A DEAD whose ROUTE_UPDATEs are still arriving: it applies with its last. */
interface Moving {
	hop: RouteHop;
	gone: boolean;
	/** How many ROUTE_UPDATEs are still to come. */
	left: bigint;
	/** The new offer of each route it moved, by Route ID. */
	offers: Map<bigint, Offer | Unusable>;
}

const same = (a: Claim | undefined, b: Claim | undefined) =>
	a === b ||
	(!!a && !!b && a.instance === b.instance && a.epoch === b.epoch && a.cost === b.cost && a.above === b.above);

// The peer's `above` for the claim at `path`, if it can be: a claim is no more than its path's
// segments broader than the path it is shown at. Mirrors rs/moq-net's `Routed::above`.
const aboveOf = (path: Path.Valid | undefined, above: bigint | undefined): bigint => {
	const value = above ?? 0n;
	if (path === undefined || value === 0n) return value;
	if (value > BigInt(Path.parts(path).length))
		throw new ProtocolViolation(`a claim at ${path} cannot be ${value} segments above it`);
	return value;
};

const through = (wire: Route, hop: RouteHop) =>
	wire.path.some((at) => at.node === hop.node && at.session === hop.session);

/** The route layer a lite-07 session's peer feeds, shared by every reader of the session. */
export class Routed {
	#session: Session;
	#readers = new Set<Reader>();
	/** The prefixes asked of the peer. */
	#asked = new Map<bigint, Asked>();
	#nextPrefix = 0n;
	/** The ANNOUNCE_REQUEST the peer has not answered yet. */
	#editing?: { remove: bigint[]; add: bigint[] };
	/** The peer finished its stream: it answers nothing more. */
	#finished = false;
	/** This side finished its stream: it asks for nothing more. */
	#silent = false;

	// Each live route and claim holds one of the session's announce slots.
	#routes: SlotMap<bigint, Route>;
	// A claim refused has no `claim` until a restart names all of it, and one refused before its
	// path has no `path` either, and never will.
	#claims: SlotMap<bigint, { route: bigint; path?: Path.Valid; claim?: Claim }>;
	// The live route to each publisher node and prefix: at most one each.
	#named = new Map<string, bigint>();
	// The routes whose committed claims include each path.
	#byPath = new Map<Path.Valid, Set<bigint>>();
	#history = new AnnounceHistory();
	#nextRoute = 0n;
	#moving?: Moving;
	// While a DEAD applies: each reader's presented paths it touched, and whether any touch there
	// lost a claim, each refreshed once it is done.
	#deferred?: Map<Reader, Map<Path.Valid, boolean>>;

	// Set once the session lets go of its routes, which then show nothing anew.
	#closing = false;
	#closed = Promise.withResolvers<void>();
	#stopDrain?: Dispose;

	constructor(session: Session) {
		this.#session = session;
		this.#routes = new SlotMap(session.slots);
		this.#claims = new SlotMap(session.slots);
		// A draining peer ranks its routes last from the GOAWAY itself.
		this.#stopDrain = session.goaway?.changed(() => {
			if (this.#closing) return;
			for (const wire of this.#routes.values()) this.#refreshRoute(wire, false);
		});
	}

	/** Feed `announced` what the stream carries for `request`, until its last consumer leaves. */
	attach(announced: announce.Producer, request: Request): void {
		if (this.#closing) {
			announced.close();
			return;
		}
		const reader: Reader = { ...request, announced, reported: new Map(), synced: false };
		this.#readers.add(reader);
		// What the stream already carries shows before the reader counts as synced.
		const paths = new Set<Path.Valid>();
		for (const wire of this.#routes.values()) {
			for (const path of wire.view.claims.keys()) {
				const at = presentedAt(reader, path);
				if (at !== undefined) paths.add(at);
			}
		}
		for (const path of paths) this.#refresh(reader, path, false);
		this.#ask();
		this.#release();
		void race([announced.closed]).then(() => {
			if (!this.#readers.delete(reader)) return;
			this.#ask();
			this.#release();
		});
	}

	// The covering set of what the readers want: no prefix at or below another. A prefix asks for
	// hidden claims when a reader beneath it does, or sits behind a hidden segment itself.
	#wanted(): Map<Path.Valid, boolean> {
		const heads = new Set<Path.Valid>();
		for (const reader of this.#readers) heads.add(reader.prefix);
		const covering = (head: Path.Valid): Path.Valid => {
			let top = head;
			for (const above of coveringPrefixes(head)) if (heads.has(above)) top = above;
			return top;
		};
		const wanted = new Map<Path.Valid, boolean>();
		for (const reader of this.#readers) {
			const top = covering(reader.prefix);
			const hidden = reader.hidden || hiddenBelow(top, reader.prefix);
			wanted.set(top, (wanted.get(top) ?? false) || hidden);
		}
		// The whole set has to fit one request: past that, everything is asked for at the root.
		if (!requestFits(wanted.keys(), this.#session.version)) {
			const hidden = [...this.#readers].some(
				(reader) => reader.hidden || hiddenBelow(Path.empty(), reader.prefix),
			);
			return new Map([[Path.empty(), hidden]]);
		}
		return wanted;
	}

	// The edit that brings what is asked of the peer in line with what the readers want.
	#edit(): { remove: bigint[]; add: Add[] } {
		const wanted = this.#wanted();
		const remove: bigint[] = [];
		for (const [id, asked] of this.#asked) {
			if (wanted.get(asked.path) === asked.hidden) wanted.delete(asked.path);
			else remove.push(id);
		}
		return { remove, add: [...wanted].map(([prefix, hidden]) => ({ prefix, hidden })) };
	}

	// Ask the peer for what the readers now want, one edit at a time.
	#ask(): void {
		if (this.#editing || this.#finished || this.#silent || this.#closing) return;
		const { remove, add } = this.#edit();
		if (remove.length === 0 && add.length === 0) return;
		const added: bigint[] = [];
		for (const { prefix, hidden } of add) {
			const id = this.#nextPrefix++;
			this.#asked.set(id, { path: prefix, hidden, live: false, synced: false });
			added.push(id);
		}
		this.#editing = { remove, add: added };
		console.debug(`announced: remove=${remove.join(",")} add=${add.map(({ prefix }) => prefix).join(",")}`);
		// However large, the edit goes whole, in as many messages as it takes.
		for (const part of requestParts(remove, add, this.#session.version)) {
			this.#session.send(part).catch((err: unknown) => this.close(error(err)));
		}
	}

	// Let each reader see its prefix synced once its answer is in: the peer answered every
	// request, and sent the initial set of each prefix asked for it.
	#release(): void {
		const done = this.#finished || this.#silent;
		if (!done) {
			if (this.#editing) return;
			const { remove, add } = this.#edit();
			if (remove.length > 0 || add.length > 0) return;
		}
		for (const reader of this.#readers) {
			if (reader.synced) continue;
			const answered = [...this.#asked.values()].every(
				(asked) =>
					asked.synced ||
					!(Path.hasPrefix(asked.path, reader.prefix) || Path.hasPrefix(reader.prefix, asked.path)),
			);
			if (!done && !answered) continue;
			reader.synced = true;
			reader.announced.synced();
		}
	}

	/** This side finished its stream: the prefixes asked of the peer stay as they are. */
	silence(): void {
		this.#silent = true;
		this.#release();
	}

	/** The peer finished its stream: it is leaving, its routes go, and it answers nothing more. */
	finish(): void {
		if (this.#moving) throw new ProtocolViolation("Announce Stream finished inside a DEAD");
		this.#finished = true;
		for (const wire of [...this.#routes.values()]) this.#endRoute(wire);
		this.#release();
	}

	/** The session ended: every reader ends with `err`, and every route and claim goes. */
	close(err?: Error): void {
		if (this.#closing) return;
		this.#closing = true;
		this.#closed.resolve();
		this.#stopDrain?.();
		for (const wire of this.#routes.values()) {
			this.#session.floors.leave(wire.node, wire.carrier);
			for (const path of wire.view.claims.keys()) this.#session.consumes.withdraw(path, wire.owner);
		}
		if (this.#routes.size > 0) this.#session.revalidate();
		this.#claims.clear();
		this.#routes.clear();
		this.#named.clear();
		this.#byPath.clear();
		const readers = [...this.#readers];
		this.#readers.clear();
		for (const reader of readers) reader.announced.close(err);
	}

	#route(id: bigint): Route {
		const found = this.#routes.get(id);
		if (!found) throw new ProtocolViolation(`unknown route ${id}`);
		return found;
	}

	#claim(id: bigint) {
		const found = this.#claims.get(id);
		if (!found) throw new ProtocolViolation(`unknown announce id ${id}`);
		return found;
	}

	// The claim at `path` under `id` is now `claim`, pending the route's next floor.
	#change(id: bigint, path: Path.Valid, claim: Claim | undefined) {
		const wire = this.#route(id);
		if (same(wire.view.claims.get(path), claim) && !wire.stale.has(path)) wire.pending.delete(path);
		else wire.pending.set(path, claim);
	}

	// Whether this node takes what the peer offers for the route at all: not a refused offer or
	// suffix, nor a route to the reserved node. Only such a route joins the node's floors.
	#joined(wire: Route): wire is Joined {
		return wire.offer !== undefined && wire.prefix !== undefined && wire.node !== 0n;
	}

	// The same, without narrowing, to compare a route before and after it changes.
	#filed(wire: Route): boolean {
		return this.#joined(wire);
	}

	// A route is no route at all once it crossed as many nodes as one may, or crossed us: a copy
	// of our own route, come back.
	#usable(wire: Route): wire is Joined {
		return (
			this.#joined(wire) &&
			wire.offer.path.length < ROUTE_MAX_HOPS &&
			!wire.offer.path.some((hop) => hop.node === this.#session.hop)
		);
	}

	// A claim through this session: its cost plus the route's metric, past this session's link
	// cost of 1 as Rust's default, ranked last while a node on its path drains, then by the
	// lowest priority on its path, past this link's 0 as Rust's default, and unavailable while
	// the route is held. Requests through it name its publisher and its prefix's depth.
	#candidate(wire: Joined, claim: Claim): WireRoute {
		return this.#session.drained(
			WireRoute.normalize({
				instance: claim.instance,
				epoch: claim.epoch,
				cost: claim.cost + wire.offer.metric + 1n,
				node: wire.node,
				depth: Path.parts(wire.prefix).length,
				priority: Math.min(0, wire.offer.priority),
				above: claim.above,
				draining: wire.offer.draining || wire.drain || undefined,
				available: !wire.held,
			}),
		);
	}

	// File the route's claim at `path` in the consume cache, or take it out. Every route's claim
	// stays there, so each consume ranks them for its own path and its epochs. `restart` marks
	// another instance or epoch, whose consumes subscribe fresh. An unusable route's claim is
	// parked, keeping its place in a tie as `byPath` does, unless it just moved to the newest.
	#serve(wire: Route, path: Path.Valid, restart: boolean) {
		const consumes = this.#session.consumes;
		const claim = wire.shown.get(path);
		if (!claim || (!this.#usable(wire) && restart)) consumes.withdraw(path, wire.owner);
		else if (!this.#usable(wire)) consumes.park(path, wire.owner);
		else consumes.announce(path, wire.owner, this.#candidate(wire, claim), restart);
	}

	// The usable claims `reader` is shown at `path`: those at the path itself, and at the
	// reader's own prefix each route's most specific claim at or above it, as the peer presents
	// a claim covering more than a requested prefix.
	#candidates(reader: Reader, path: Path.Valid): WireRoute[] {
		const candidates: WireRoute[] = [];
		const seen = new Set<bigint>();
		for (const at of path === reader.prefix ? coveringPrefixes(path) : [path]) {
			for (const id of this.#byPath.get(at) ?? []) {
				const wire = this.#route(id);
				const claim = wire.shown.get(at);
				if (!claim || seen.has(id)) continue;
				seen.add(id);
				// A claim above the path covers it only where the newest floor certifying the path
				// leaves it, as a consume of the path picks.
				if (!this.#session.floors.covers(wire.node, at, path)) continue;
				if (this.#usable(wire)) candidates.push(this.#candidate(wire, claim));
			}
		}
		return candidates;
	}

	// Report to `reader` the best claim at `path`, or with `perNode` each origin node's, and each
	// one's end. `lost` marks the routes leaving as let go rather than withdrawn, so they go
	// offline rather than ending.
	#refresh(reader: Reader, path: Path.Valid, lost: boolean) {
		const { announced, scope } = reader;
		if (announced.closed.peek() !== undefined) return;
		// Keyed by origin node with `perNode`, and otherwise the one key `undefined`.
		const now = new Map<bigint | undefined, WireRoute>();
		if (visible(reader, path)) {
			const candidates = this.#candidates(reader, path);
			if (reader.perNode) {
				for (const next of candidates) now.set(next.node, next);
			} else {
				// The same pick a consume of `path` makes from the cache.
				const best = bestCandidate(path, path, candidates, (next) => next);
				if (best) now.set(undefined, best);
			}
		}

		const captures = scopeCaptures(scope, path);
		const prev = reader.reported.get(path) ?? new Map<bigint | undefined, WireRoute>();
		// Arrivals first, so a reader sees a claim replaced rather than the path gone between.
		for (const [key, next] of now) {
			const was = prev.get(key);
			if (was && routesEqual(was, next)) continue;
			const restart = !!was && (was.instance !== next.instance || was.epoch !== next.epoch);
			const kind = !was ? "start" : restart ? "restart" : "update";
			console.debug(`announced: broadcast=${path} ${kind} instance=${next.instance} node=${next.node}`);
			announced.append({ prefix: path, captures, kind, route: next });
		}
		for (const [key, was] of prev) {
			if (now.has(key)) continue;
			console.debug(`announced: broadcast=${path} active=false lost=${lost} node=${was.node}`);
			announced.append({ prefix: path, captures, kind: lost ? "offline" : "end", route: was });
		}
		if (now.size > 0) reader.reported.set(path, now);
		else reader.reported.delete(path);
	}

	// Report the claims at `path` to every reader it shows under: at the path itself for a reader
	// above it, and at the reader's own prefix for one below.
	// What covers a path changed: every consume picks anew, and each reader is shown again
	// what covers its own prefix from above it.
	#revalidate() {
		this.#session.revalidate();
		for (const reader of this.#readers) this.#touch(reader.prefix, false);
	}

	#touch(path: Path.Valid, lost: boolean) {
		for (const reader of this.#readers) {
			const at = presentedAt(reader, path);
			if (at === undefined) continue;
			if (!this.#deferred) {
				this.#refresh(reader, at, lost);
				continue;
			}
			// A path touched again keeps a loss: a later touch only rechecks it.
			const paths = this.#deferred.get(reader) ?? new Map<Path.Valid, boolean>();
			paths.set(at, (paths.get(at) ?? false) || lost);
			this.#deferred.set(reader, paths);
		}
	}

	// Show or hide the route's claims at `touched`, each marked whether it restarted, or with
	// `all` every claim it holds, once another route's floor may have moved what is current.
	#show(wire: Route, touched: ReadonlyMap<Path.Valid, boolean>, all: boolean) {
		const floors = this.#session.floors;
		const paths = all
			? new Set([...touched.keys(), ...wire.shown.keys(), ...wire.view.claims.keys()])
			: touched.keys();
		for (const path of paths) {
			const now = floors.shows(wire.node, wire.view, path);
			const was = wire.shown.get(path);
			const restart = touched.get(path);
			if (same(now, was) && restart === undefined) continue;
			if (now) wire.shown.set(path, now);
			else wire.shown.delete(path);
			// A claim shown anew or restarted iterates last, as the newest; a re-price keeps its
			// place, as the consume cache does. See bestCandidate.
			const ids = this.#byPath.get(path);
			if (now && ids && (!was || restart)) {
				ids.delete(wire.id);
				ids.add(wire.id);
			}
			this.#serve(wire, path, restart ?? false);
			// A claim gone was let go, not withdrawn by its origin, where the newest floor certifying it
			// still presents it, or where this node refuses one of the route's claims and no floor
			// certifies it gone. An uncertified withdrawal otherwise still ends: a relay certifies
			// nothing to a request wider than its scope's region, as this one may be.
			const said = now ? undefined : floors.certified(wire.node, wire.view, path);
			const refusing = wire.unnamed.size > 0 || wire.withheld.size > 0;
			this.#touch(path, !now && (said === "present" || (refusing && said === undefined)));
		}
	}

	#refreshRoute(wire: Route, lost: boolean) {
		for (const path of wire.view.claims.keys()) {
			this.#serve(wire, path, false);
			this.#touch(path, lost);
		}
	}

	#key(node: bigint, at: Path.Valid | undefined): string | undefined {
		return at === undefined || node === 0n ? undefined : `${node} ${at}`;
	}

	// A route with nothing committed, whose claims another route's floor may hide.
	#blank(
		id: bigint,
		from: Pick<Route, "node" | "path" | "offer" | "drain" | "held">,
		at: {
			region: Path.Valid;
			prefix: Path.Valid | undefined;
			exact: boolean;
			hidden: boolean;
			revision: bigint | undefined;
		},
	): Route {
		const view: Route["view"] = {
			prefix: at.prefix ?? at.region,
			hidden: at.hidden,
			revision: at.revision,
			claims: new Map(),
		};
		const wire: Route = {
			id,
			node: from.node,
			region: at.region,
			prefix: at.prefix,
			exact: at.exact,
			path: from.path,
			offer: from.offer,
			drain: from.drain,
			held: from.held,
			claims: new Map(),
			unnamed: new Set(),
			withheld: new Set(),
			pending: new Map(),
			stale: new Set(),
			view,
			shown: new Map(),
			carrier: {
				view,
				// Another route's floor or end runs this: a closing session is about to let go.
				reshow: () => {
					if (!this.#closing) this.#show(wire, new Map(), true);
				},
			},
			owner: {},
		};
		return wire;
	}

	// The peer started a route to `node` with the next Route ID, covering the requested prefix
	// with the Prefix ID `asked` joined with `suffix`, at its own prefix when `exact`, along
	// `offer`, or along `path` when this node refuses the offer.
	#startRoute(
		node: bigint,
		asked: bigint,
		suffix: Path.Valid | undefined,
		exact: boolean,
		offer: Offer | undefined,
		path: RouteHop[],
	) {
		// The peer names a prefix in effect on its stream.
		const region = this.#asked.get(asked);
		if (!region?.live) throw new ProtocolViolation(`unknown Prefix ID ${asked}`);
		// Fields valid alone can still join the prefix past the path limits: refused too.
		const joined = suffix === undefined ? undefined : Path.join(region.path, suffix);
		const at = joined !== undefined && Path.check(joined) === undefined ? joined : undefined;
		const named = this.#key(node, at);
		// The peer keeps one route per publisher and prefix.
		if (named !== undefined && this.#named.has(named))
			throw new ProtocolViolation(`second route to node ${node} at ${at}`);
		const id = this.#nextRoute++;
		const wire = this.#blank(
			id,
			{ node, path, offer, drain: false, held: false },
			{ region: region.path, prefix: at, exact, hidden: region.hidden, revision: undefined },
		);
		this.#routes.set(id, wire);
		if (named !== undefined) this.#named.set(named, id);
		// A refused route's floors stay its own, as Rust's node never holds the route.
		if (this.#joined(wire)) this.#session.floors.join(node, wire.carrier);
	}

	// The peer let go of the route and every claim under it: offline, not withdrawn.
	#endRoute(wire: Route) {
		for (const id of [...wire.claims.values(), ...wire.unnamed]) {
			this.#history.end(id);
			this.#claims.delete(id);
		}
		this.#routes.delete(wire.id);
		const named = this.#key(wire.node, wire.prefix);
		if (named !== undefined && this.#named.get(named) === wire.id) this.#named.delete(named);
		this.#drop(wire);
		this.#revalidate();
	}

	// Take a route that no longer lives out of what the session shows: offline, not withdrawn.
	#drop(wire: Route) {
		// Unindexed first: leaving its floors reshows another carrier at once, which refreshes
		// these paths and so looks up every route indexed there.
		for (const path of wire.view.claims.keys()) this.#unindex(path, wire.id);
		// What its last floor certified stays, so no other route's lagging claim revives.
		this.#session.floors.leave(wire.node, wire.carrier);
		wire.shown.clear();
		this.#refreshRoute(wire, true);
	}

	#index(path: Path.Valid, id: bigint) {
		let ids = this.#byPath.get(path);
		if (!ids) {
			ids = new Set();
			this.#byPath.set(path, ids);
		}
		ids.add(id);
	}

	#unindex(path: Path.Valid, id: bigint) {
		const ids = this.#byPath.get(path);
		ids?.delete(id);
		if (ids?.size === 0) this.#byPath.delete(path);
	}

	// A claim the peer started under `routeId` with the next Announce ID, at `path` unless this
	// node refused it, and `claim` unless it refuses what the peer forwarded.
	#startClaim(routeId: bigint, path: { suffix: Path.Valid; pathBase?: Base } | undefined, claim: Claim | undefined) {
		const wire = this.#route(routeId);
		// Every claim's path is relative to the session, whichever requested prefix it lies in.
		const named = this.#history.start(path);
		const id = this.#history.next - 1n;
		if (claim) claim.above = aboveOf(named, claim.above);
		if (named === undefined) {
			if (claim) console.warn(`refusing a forwarded claim: route=${routeId} claim=${id} reason=path`);
			this.#claims.set(id, { route: routeId });
			wire.unnamed.add(id);
			return;
		}
		if (wire.claims.has(named)) throw new ProtocolViolation(`route ${routeId} claims ${named} twice`);
		// A route covers only the claims at or below its prefix.
		if (wire.prefix !== undefined && !Path.hasPrefix(wire.prefix, named))
			throw new ProtocolViolation(`route ${routeId} at ${wire.prefix} claims ${named}`);
		this.#claims.set(id, { route: routeId, path: named, claim });
		wire.claims.set(named, id);
		if (claim) this.#change(routeId, named, claim);
		else wire.withheld.add(id);
	}

	// The route now reaches its node by `offer`, or along `path` while this node refuses what the
	// peer forwarded. Either way the peer no longer holds it, nor drains a node on it but as the
	// offer says. A refused route leaves the node's floors, as an ended one does, and joins again
	// once usable, with what its last floor certified, as rs/moq-net files it again.
	#reach(wire: Route, offer: Offer | undefined, path: RouteHop[]) {
		const floors = this.#session.floors;
		const was = this.#filed(wire);
		wire.offer = offer;
		wire.path = path;
		wire.held = false;
		wire.drain = false;
		const now = this.#filed(wire);
		if (was && !now) {
			floors.leave(wire.node, wire.carrier);
			// What its floor certified stays as it was, while the route goes on reading claims.
			wire.view.claims = new Map(wire.view.claims);
		}
		if (was || !now) return;
		floors.join(wire.node, wire.carrier);
		const others = floors.floor(wire.node, wire.carrier);
		this.#show(wire, new Map(), true);
		if (others) this.#revalidate();
	}

	// The route moved to `offer`, which this node refuses with its path when it is `Unusable`.
	#move(wire: Route, offer: Offer | Unusable) {
		const was = this.#filed(wire);
		if ("refused" in offer) {
			console.warn(`refusing a forwarded route: node=${wire.node} route=${wire.id} reason=${offer.refused}`);
			this.#reach(wire, undefined, offer.path);
		} else {
			this.#reach(wire, offer, offer.path);
		}
		this.#refreshRoute(wire, !this.#usable(wire));
		// Its floors no longer cover anything, as on ROUTE_END.
		if (was && !this.#filed(wire)) this.#revalidate();
	}

	// A DEAD: the stream's routes through the hop it names carry on as it says, once the
	// ROUTE_UPDATEs it counts are in. A route this node refuses still follows it, so both ends
	// agree on which routes live. An endpoint casts no doubt: it never forwards a route, so no
	// detour waits on one.
	#dead(news: Dead) {
		if (news.kind === "drain") {
			for (const wire of this.#routes.values()) {
				if (!wire.path.some((hop) => hop.node === news.node)) continue;
				wire.drain = true;
				this.#refreshRoute(wire, false);
			}
			return;
		}
		// It moves only routes through the hop, so what its updates hold stays within the stream's.
		let crossing = 0n;
		for (const wire of this.#routes.values()) if (through(wire, news.hop)) crossing++;
		if (news.moved > crossing) throw new ProtocolViolation(`DEAD moves ${news.moved} of ${crossing} routes`);
		const moving = { hop: news.hop, gone: news.gone, left: news.moved, offers: new Map() };
		if (moving.left === 0n) this.#applyDead(moving);
		else this.#moving = moving;
	}

	// One of the ROUTE_UPDATEs a DEAD counted: it names a live route through the hop, once.
	#moved(moving: Moving, msg: AnnounceBroadcast) {
		let id: bigint;
		let offer: Offer | Unusable;
		if (msg.status === "routeUpdate") {
			id = msg.id;
			offer = msg.offer;
		} else if (msg.status === "refused" && msg.message.kind === "routeUpdate") {
			id = msg.message.id;
			offer = { refused: msg.reason, path: msg.message.path };
		} else {
			throw new ProtocolViolation(`${msg.status} inside a DEAD's ROUTE_UPDATEs`);
		}
		const wire = this.#routes.get(id);
		if (!wire || !through(wire, moving.hop)) throw new ProtocolViolation(`DEAD moves route ${id} off its hop`);
		if (moving.offers.has(id)) throw new ProtocolViolation(`DEAD moves route ${id} twice`);
		moving.offers.set(id, offer);
		moving.left -= 1n;
		if (moving.left > 0n) return;
		this.#moving = undefined;
		this.#applyDead(moving);
	}

	// Apply a DEAD and the offers its ROUTE_UPDATEs gave as one: readers and consumes see only
	// where every route it moves ends up.
	#applyDead(moving: Moving) {
		const touched = new Map<Reader, Map<Path.Valid, boolean>>();
		this.#deferred = touched;
		try {
			this.#session.consumes.batch(() => this.#deadRoutes(moving));
		} finally {
			this.#deferred = undefined;
		}
		for (const [reader, paths] of touched) {
			for (const [at, lost] of paths) this.#refresh(reader, at, lost);
		}
	}

	#deadRoutes({ hop, gone, offers }: Moving) {
		const crossing = [...this.#routes.values()].filter((wire) => through(wire, hop));
		for (const wire of crossing) {
			const moved = offers.get(wire.id);
			const before = wire.path.at(-2);
			if (moved !== undefined) this.#move(wire, moved);
			else if (gone) this.#endRoute(wire);
			// The peer learned it from this node, which it holds under: unchanged.
			else if (before?.node === this.#session.hop) continue;
			else {
				wire.held = true;
				this.#refreshRoute(wire, false);
			}
		}
	}

	// Apply a message whose content, which the peer only forwarded, this node refuses: withdraw
	// the route or claim it names, and keep the stream. Mirrors rs/moq-net.
	#refused(message: Refusable, reason: AnnounceRefusal) {
		switch (message.kind) {
			case "routeStart":
				console.warn(`refusing a forwarded route: node=${message.node} reason=${reason}`);
				return this.#startRoute(
					message.node,
					message.prefix,
					message.suffix,
					message.exact,
					undefined,
					message.path,
				);
			case "routeUpdate":
				return this.#move(this.#route(message.id), { refused: reason, path: message.path });
			case "start":
				console.warn(
					`refusing a forwarded claim: route=${message.route} claim=${this.#history.next} reason=${reason}`,
				);
				return this.#startClaim(message.route, message.path, undefined);
			case "change": {
				const live = this.#claim(message.id);
				console.warn(`refusing a forwarded claim: route=${live.route} claim=${message.id} reason=${reason}`);
				live.claim = undefined;
				if (live.path === undefined) return;
				this.#route(live.route).withheld.add(message.id);
				this.#change(live.route, live.path, undefined);
				return;
			}
		}
	}

	// ANNOUNCE_FLOOR: the route's batch applies whole, at the revision its floor certifies: none
	// while this node refuses one of the route's claims, which its origin never withdrew.
	#floor(id: bigint, floor: bigint | undefined) {
		const floors = this.#session.floors;
		const wire = this.#route(id);
		const revision = wire.unnamed.size === 0 && wire.withheld.size === 0 ? floor : undefined;
		// A floor that certifies less keeps what the route's last one certified.
		const certified = wire.view.revision;
		const filed = this.#filed(wire);
		if (filed && certified !== undefined && (revision === undefined || revision < certified))
			floors.regress(wire.node, wire.carrier);
		// The floor says what covers the prefix a widening edit took the route's claim from.
		for (const path of wire.stale) if (!wire.pending.has(path)) wire.pending.set(path, undefined);
		wire.stale.clear();
		const touched = new Map<Path.Valid, boolean>();
		for (const [path, claim] of wire.pending) {
			// Another instance or epoch: what was consumed through the claim may be stale.
			const was = wire.view.claims.get(path);
			const restart = !!was && !!claim && (was.instance !== claim.instance || was.epoch !== claim.epoch);
			touched.set(path, restart);
			if (claim) {
				wire.view.claims.set(path, claim);
				this.#index(path, id);
			} else {
				wire.view.claims.delete(path);
				this.#unindex(path, id);
			}
		}
		wire.pending.clear();
		wire.view.revision = revision;
		// The floor answers the requested prefix as it stands, hidden claims included or not.
		wire.view.hidden = this.#hidden(wire.region);
		// Every other route to the node is shown anew first; with one, this route's unchanged
		// claims may change state too.
		const others = filed && floors.floor(wire.node, wire.carrier);
		this.#show(wire, touched, others);
		// Another view of the node may now cover a consumed path differently.
		if (others) this.#revalidate();
	}

	// Whether the requested prefix `region` asks for hidden claims.
	#hidden(region: Path.Valid): boolean {
		for (const asked of this.#asked.values()) if (asked.live && asked.path === region) return asked.hidden;
		return false;
	}

	// ANNOUNCE_OK: the last edit takes effect on the peer's stream. Its live routes carry over as
	// `rekey.plan` says, which the peer applies alike, so nothing both sets cover is sent again.
	#ok() {
		const editing = this.#editing;
		if (!editing) throw new ProtocolViolation("ANNOUNCE_OK with no ANNOUNCE_REQUEST unanswered");
		// The marker lies between batches.
		for (const wire of this.#routes.values()) {
			if (wire.pending.size > 0) throw new ProtocolViolation("ANNOUNCE_OK inside a batch");
		}
		this.#editing = undefined;
		const old = new Set<Path.Valid>();
		for (const asked of this.#asked.values()) if (asked.live) old.add(asked.path);
		for (const id of editing.remove) this.#asked.delete(id);
		const added: Path.Valid[] = [];
		for (const id of editing.add) {
			const asked = this.#asked.get(id);
			if (!asked) throw new Error("an added prefix");
			asked.live = true;
			added.push(asked.path);
		}
		const next = new Set<Path.Valid>();
		for (const asked of this.#asked.values()) next.add(asked.path);

		const table = new Map<bigint, rekey.Route>();
		for (const [id, wire] of this.#routes) {
			table.set(id, {
				node: wire.node,
				prefix: wire.prefix ?? wire.region,
				region: wire.region,
				exact: wire.exact,
				order: rekey.order(wire.held, wire.offer, wire.path, this.#session.hop),
				claims: wire.claims,
			});
		}
		this.#carry(rekey.plan(old, next, added, table, this.#nextRoute, this.#history.next));

		// What the readers want may have moved while the peer answered.
		this.#ask();
		this.#release();
	}

	// Carry the stream's routes over an edit as `plan` says: a route that stays keeps everything,
	// one that widens or narrows is filed again under its new prefix, and every other route ends.
	#carry(plan: rekey.Plan) {
		const { floors, consumes } = this.#session;
		const sources = new Map(this.#routes);
		const records = new Map(this.#claims);
		// The routes filed again, the ones they replace or the edit ends, and the claims that live on.
		const fresh: Route[] = [];
		const ended: Route[] = [];
		const stays = new Set<bigint>();
		const placed = new Map<bigint, { route: bigint; path?: Path.Valid; claim?: Claim }>();
		// The claims an edit presents below their own path, in the order it assigned their ids.
		const inserted: { id: bigint; path: Path.Valid }[] = [];
		// The routes the plan carries on, each of which keeps its own claims.
		const carriedFrom = new Set(plan.routes.map((carried) => carried.from));

		for (const carried of plan.routes) {
			const source = sources.get(carried.from);
			if (!source) throw new Error("a carried route's source");
			if (carried.kind === "stays") {
				stays.add(source.id);
				for (const id of [...source.claims.values(), ...source.unnamed]) {
					const record = records.get(id);
					if (record) placed.set(id, record);
				}
				source.region = carried.region;
				// A prefix that no longer asks for hidden claims certifies nothing about them.
				if (!this.#hidden(carried.region)) source.view.hidden = false;
				continue;
			}
			const wire = this.#blank(carried.id, source, {
				region: carried.region,
				prefix: source.prefix === undefined ? undefined : carried.prefix,
				exact: carried.exact,
				hidden: source.view.hidden && this.#hidden(carried.region),
				// A widened route's floors certified only the narrower prefix.
				revision: carried.kind === "widens" ? undefined : source.view.revision,
			});
			for (const [path, claim] of carried.claims) {
				const record = records.get(claim.from);
				if (!record) throw new Error("a carried claim's record");
				// A claim now covering a narrower prefix from above is that much more above it.
				const held =
					record.claim && claim.lowered > 0n
						? { ...record.claim, above: record.claim.above + claim.lowered }
						: record.claim;
				wire.claims.set(path, claim.id);
				if (held) wire.view.claims.set(path, held);
				else wire.withheld.add(claim.id);
				placed.set(claim.id, { route: wire.id, path, claim: held });
				if (claim.id !== claim.from) inserted.push({ id: claim.id, path });
			}
			if (carried.kind === "widens") {
				// A claim without a path stays with the route it was sent under.
				for (const id of source.unnamed) {
					wire.unnamed.add(id);
					placed.set(id, { route: wire.id });
				}
				// The claim at each merged route's old prefix shows until the next floor says what
				// covers it now: the routes the edit merged into this one end without a successor.
				const merged = [...sources.values()].filter(
					(other) =>
						other.id === source.id ||
						(!carriedFrom.has(other.id) &&
							other.node === source.node &&
							Path.hasPrefix(carried.region, other.region)),
				);
				for (const other of merged) {
					const own = other.prefix === undefined ? undefined : other.view.claims.get(other.prefix);
					if (!own || other.prefix === undefined || wire.view.claims.has(other.prefix)) continue;
					wire.view.claims.set(other.prefix, own);
					wire.stale.add(other.prefix);
				}
			}
			fresh.push(wire);
		}

		// Let go of what the edit ends before taking the slots of what it starts.
		for (const id of [...this.#claims.keys()]) {
			if (placed.has(id)) continue;
			this.#history.end(id);
			this.#claims.delete(id);
		}
		const replaced = new Set(fresh.map((wire) => wire.id));
		for (const [id, wire] of sources) {
			if (stays.has(id)) continue;
			ended.push(wire);
			if (!replaced.has(id)) this.#routes.delete(id);
		}
		for (const { id, path } of inserted) {
			if (this.#history.insert(path) !== id) throw new Error("the edit assigned another Announce ID");
		}
		if (this.#history.next !== plan.nextClaim) throw new Error("the edit assigned other Announce IDs");
		for (const [id, record] of placed) this.#claims.set(id, record);
		for (const wire of fresh) this.#routes.set(wire.id, wire);
		this.#nextRoute = plan.nextRoute;
		this.#named.clear();
		for (const [id, wire] of this.#routes) {
			const named = this.#key(wire.node, wire.prefix);
			if (named !== undefined) this.#named.set(named, id);
		}

		// Every path a reader may be shown differently, once the routes are filed again.
		const touched = new Set<Path.Valid>();
		const shows = (wire: Route) => {
			for (const path of wire.view.claims.keys()) {
				const claim = floors.shows(wire.node, wire.view, path);
				if (claim) wire.shown.set(path, claim);
				else wire.shown.delete(path);
			}
		};
		// The routes filed again show before the ones they replace go, so neither a reader nor a
		// consume of a claim both carry finds it gone between.
		for (const wire of fresh) {
			for (const path of wire.view.claims.keys()) {
				this.#index(path, wire.id);
				touched.add(path);
			}
			if (this.#joined(wire)) floors.join(wire.node, wire.carrier);
			shows(wire);
		}
		for (const wire of ended) {
			// A widened route keeps its Route ID, and with it the entry of each claim it carried over.
			const kept = replaced.has(wire.id) ? this.#routes.get(wire.id) : undefined;
			for (const path of wire.view.claims.keys()) {
				if (!kept?.view.claims.has(path)) this.#unindex(path, wire.id);
				touched.add(path);
			}
		}
		for (const wire of ended) floors.leave(wire.node, wire.carrier);
		for (const wire of fresh) {
			shows(wire);
			for (const path of wire.view.claims.keys()) this.#serve(wire, path, false);
		}
		for (const wire of ended) {
			for (const path of wire.view.claims.keys()) consumes.withdraw(path, wire.owner);
		}
		if (fresh.length > 0 || ended.length > 0) this.#revalidate();
		// A claim no route carries any more was let go, not withdrawn by its origin.
		for (const path of touched) this.#touch(path, true);
	}

	/** Apply one message the peer sent on its Announce Stream as a publisher. */
	async receive(msg: AnnounceBroadcast): Promise<void> {
		if (this.#closing) return;
		// A DEAD's ROUTE_UPDATEs arrive back to back.
		if (this.#moving) return this.#moved(this.#moving, msg);

		switch (msg.status) {
			case "ok":
				this.#ok();
				break;
			case "synced": {
				const asked = this.#asked.get(msg.prefix);
				if (!asked?.live) throw new ProtocolViolation(`SYNCED for unknown Prefix ID ${msg.prefix}`);
				if (asked.synced) throw new ProtocolViolation(`duplicate SYNCED for Prefix ID ${msg.prefix}`);
				asked.synced = true;
				this.#release();
				break;
			}
			case "request":
				// The peer's own request is answered by the publisher half: here it only has to
				// arrive outside a DEAD's updates.
				break;
			case "routeStart":
				this.#startRoute(msg.node, msg.prefix, msg.suffix, msg.exact, msg.offer, msg.offer.path);
				break;
			case "routeUpdate":
				this.#move(this.#route(msg.id), msg.offer);
				break;
			case "routeHold": {
				const wire = this.#route(msg.id);
				wire.held = true;
				this.#refreshRoute(wire, false);
				break;
			}
			case "routeEnd":
				this.#endRoute(this.#route(msg.id));
				break;
			case "dead":
				this.#dead(msg.dead);
				break;
			case "floor":
				this.#floor(msg.id, msg.revision);
				break;
			case "active": {
				if (msg.route === undefined) throw new ProtocolViolation("lite-07 claim without a route");
				if (msg.instance === undefined) throw new ProtocolViolation("lite-07 claim without an instance");
				const claim = {
					instance: msg.instance,
					epoch: msg.epoch ?? 0n,
					cost: msg.cost ?? Cost.zero,
					above: msg.above ?? 0n,
				};
				this.#startClaim(msg.route, msg, claim);
				break;
			}
			case "update": {
				const live = this.#claim(msg.id);
				const above = aboveOf(live.path, msg.above);
				// An update names none of what a refused claim lacks.
				if (live.claim === undefined || live.path === undefined) break;
				live.claim = { ...live.claim, cost: msg.cost ?? Cost.zero, above };
				this.#change(live.route, live.path, live.claim);
				break;
			}
			case "restart": {
				const live = this.#claim(msg.id);
				if (msg.instance === undefined) throw new ProtocolViolation("lite-07 restart without an instance");
				// A claim without a path stays refused, whatever the peer sends for it.
				if (live.path === undefined) break;
				live.claim = {
					instance: msg.instance,
					epoch: msg.epoch ?? 0n,
					cost: msg.cost ?? Cost.zero,
					above: aboveOf(live.path, msg.above),
				};
				this.#route(live.route).withheld.delete(msg.id);
				this.#change(live.route, live.path, live.claim);
				break;
			}
			case "refused":
				this.#refused(msg.message, msg.reason);
				break;
			case "endedId": {
				const live = this.#claim(msg.id);
				this.#history.end(msg.id);
				this.#claims.delete(msg.id);
				const wire = this.#route(live.route);
				if (live.path === undefined) {
					wire.unnamed.delete(msg.id);
					break;
				}
				wire.claims.delete(live.path);
				wire.withheld.delete(msg.id);
				this.#change(live.route, live.path, undefined);
				break;
			}
			case "skipped":
				break;
			case "ended":
				// Lite-07 ends a claim by its Announce ID only.
				throw new ProtocolViolation("ANNOUNCE_END by path on lite-07");
		}
	}
}

// Whether `reader` reports a claim at `path`.
function visible(reader: Reader, path: Path.Valid): boolean {
	const matches = reader.covering ? scopeOverlaps(reader.scope, path) : reader.scope.matches(path);
	return matches && (reader.hidden || !hiddenBelow(reader.prefix, path));
}

// Where `reader` is shown a claim at `path`: there when it lies within the reader's prefix, at
// the prefix itself when it covers more, and nowhere otherwise.
function presentedAt(reader: Reader, path: Path.Valid): Path.Valid | undefined {
	if (Path.hasPrefix(reader.prefix, path)) return path;
	return Path.hasPrefix(path, reader.prefix) ? reader.prefix : undefined;
}

// Lite-07: how an Announce Stream's live routes carry over an edit of its requested prefixes, at
// the ANNOUNCE_OK answering it.
//
// Both endpoints run `plan` over what the stream carries, so they agree on which routes and claims
// live on, and under which ids, without a message for any of it. Mirrors `lite/rekey.rs` in
// rs/moq-net.

import { coveringPrefixes } from "../internal.ts";
import * as Path from "../path.ts";
import type { Offer, RouteHop } from "./announce.ts";

/**
 * How a route ranks among the routes that would share an origin node and prefix: one that is not
 * held first, then one whose path does not name the subscriber, then the better rank (not
 * draining, the higher priority, the lower metric).
 */
export interface Order {
	held: boolean;
	names: boolean;
	draining: boolean;
	priority: number;
	metric: bigint;
}

/** The {@link Order} of a route offering `offer`, or `path` alone for an offer the receiver refused,
 * to the subscriber `receiver`. */
export function order(held: boolean, offer: Offer | undefined, path: RouteHop[], receiver: bigint): Order {
	const names = path.some((hop) => hop.node === receiver);
	if (offer) return { held, names, draining: offer.draining, priority: offer.priority, metric: offer.metric };
	return { held, names, draining: true, priority: -128, metric: 2n ** 64n - 1n };
}

/** Negative when `a` is preferred to `b`, positive when `b` is, and 0 when they rank alike. */
export function compare(a: Order, b: Order): number {
	if (a.held !== b.held) return a.held ? 1 : -1;
	if (a.names !== b.names) return a.names ? 1 : -1;
	if (a.draining !== b.draining) return a.draining ? 1 : -1;
	if (a.priority !== b.priority) return b.priority - a.priority;
	return a.metric < b.metric ? -1 : a.metric > b.metric ? 1 : 0;
}

/** A live route on the stream. */
export interface Route {
	node: bigint;
	/** The prefix it covers, or the requested prefix it lies in when its suffix was refused. */
	prefix: Path.Valid;
	/** The requested prefix it lies in. */
	region: Path.Valid;
	/** Its ROUTE_START said its sender keeps it at its own prefix, so a widening leaves it there
	 * rather than moving it up. */
	exact: boolean;
	order: Order;
	/** The Announce ID of each of its live claims with a path. */
	claims: ReadonlyMap<Path.Valid, bigint>;
}

/**
 * What became of a route that lives on. `stays`: unchanged. `widens`: moved to a prefix above the
 * one it was requested under, so it certifies nothing until its next floor. `narrows`: carried to
 * a prefix below the one it covered, with its floor.
 */
export type Kind = "stays" | "widens" | "narrows";

/** A claim of a route that lives on: its Announce ID from here on, and the one it had, the same
 * unless it is now presented at a prefix below its path. `lowered` is how many segments below where
 * it was presented it is presented now, which its Above grows by: 0 unless it now covers a narrower
 * prefix from above. */
export interface Claim {
	id: bigint;
	from: bigint;
	lowered: bigint;
}

/** A route that lives on: its Route ID from here on, and the route whose offer and state it keeps. */
export interface Carried {
	id: bigint;
	from: bigint;
	kind: Kind;
	prefix: Path.Valid;
	region: Path.Valid;
	/** It lies at its own prefix: one that stays keeps it, and one that widens or narrows is
	 * presented at a prefix that is not its own. */
	exact: boolean;
	claims: Map<Path.Valid, Claim>;
}

/** The routes that live on, by their new Route ID. Every other route ends, and every Announce ID
 * no claim here keeps retires. */
export interface Plan {
	routes: Carried[];
	nextRoute: bigint;
	nextClaim: bigint;
}

/**
 * Carry `routes` over an edit from the prefixes `old` to `next`, which added `added` in that
 * order. `nextRoute` and `nextClaim` are the next unassigned Route ID and Announce ID.
 */
export function plan(
	old: ReadonlySet<Path.Valid>,
	next: ReadonlySet<Path.Valid>,
	added: readonly Path.Valid[],
	routes: ReadonlyMap<bigint, Route>,
	nextRoute: bigint,
	nextClaim: bigint,
): Plan {
	const region = (prefix: Path.Valid): Path.Valid | undefined => {
		for (const above of coveringPrefixes(prefix)) if (next.has(above)) return above;
		return undefined;
	};
	const key = (node: bigint, prefix: Path.Valid) => `${node} ${prefix}`;
	const ids = [...routes.keys()].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
	const route = (id: bigint): Route => {
		const found = routes.get(id);
		if (!found) throw new Error(`no route ${id}`);
		return found;
	};

	const stays = new Map<string, bigint>();
	const widening = new Map<string, { wider: Path.Valid; sources: bigint[] }>();
	const narrowing: { from: bigint; inside: Path.Valid }[] = [];
	for (const id of ids) {
		const { node, prefix, region: lies, exact } = route(id);
		if (!old.has(lies)) throw new Error("a route lies in a requested prefix");
		const covered = region(prefix) !== undefined;
		const wider = region(lies);
		// A route at its own prefix stays there, as one strictly inside the region does.
		if (covered && (next.has(lies) || prefix !== lies || exact)) {
			stays.set(key(node, prefix), id);
		} else if (wider !== undefined) {
			const entry = widening.get(key(node, wider));
			if (entry) entry.sources.push(id);
			else widening.set(key(node, wider), { wider, sources: [id] });
		} else if (prefix === lies) {
			for (const inside of added) {
				if (inside !== lies && Path.hasPrefix(lies, inside)) narrowing.push({ from: id, inside });
			}
		}
	}

	const carried = new Map<bigint, Carried>();
	const kept = (claims: ReadonlyMap<Path.Valid, bigint>): [Path.Valid, Claim][] =>
		[...claims].map(([path, id]) => [path, { id, from: id, lowered: 0n }]);
	for (const { wider, sources } of widening.values()) {
		let winner = sources[0];
		for (const id of sources) if (compare(route(id).order, route(winner).order) < 0) winner = id;
		const claims = new Map<Path.Valid, Claim>();
		for (const id of sources) {
			const source = route(id);
			for (const [path, claim] of kept(source.claims)) if (path !== source.prefix) claims.set(path, claim);
		}
		carried.set(winner, {
			id: winner,
			from: winner,
			kind: "widens",
			prefix: wider,
			region: wider,
			exact: false,
			claims,
		});
	}
	for (const { from, inside } of narrowing) {
		const source = route(from);
		const at = key(source.node, inside);
		// A route already there stays unless this one would be preferred.
		const staying = stays.get(at);
		if (staying !== undefined) {
			if (compare(route(staying).order, source.order) <= 0) continue;
			stays.delete(at);
		}
		const claims = new Map<Path.Valid, Claim>();
		// Of the claims covering the whole prefix alike, only the most specific shows.
		let above: [Path.Valid, bigint] | undefined;
		for (const [path, id] of source.claims) {
			if (path !== inside && Path.hasPrefix(inside, path)) claims.set(path, { id, from: id, lowered: 0n });
			if (Path.hasPrefix(path, inside) && (!above || path.length > above[0].length)) above = [path, id];
		}
		if (above) {
			const id = above[0] === inside ? above[1] : nextClaim++;
			const lowered = BigInt(Path.parts(inside).length - Path.parts(above[0]).length);
			claims.set(inside, { id, from: above[1], lowered });
		}
		const id = nextRoute++;
		carried.set(id, { id, from, kind: "narrows", prefix: inside, region: inside, exact: false, claims });
	}
	for (const id of stays.values()) {
		const { prefix, exact, claims } = route(id);
		const lies = region(prefix);
		if (lies === undefined) throw new Error("a route that stays is covered");
		carried.set(id, { id, from: id, kind: "stays", prefix, region: lies, exact, claims: new Map(kept(claims)) });
	}
	return {
		routes: [...carried.values()].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)),
		nextRoute,
		nextClaim,
	};
}

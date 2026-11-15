import { ProtocolViolation } from "../error.ts";
import { type Cost, type Hop, MAX_HOPS, toHop, UNKNOWN_HOP, wireHop } from "../hop.ts";
import type * as Instance from "../instance.ts";
import { coveringPrefixes } from "../internal.ts";
import * as Path from "../path.ts";
import { encodeVarint, type Reader, type Writer } from "../stream.ts";
import { SEGMENT_BYTES } from "../util/limits.ts";
import { decodeForwarded, decodeRoute, encodeRoute, utf8 } from "./instance.ts";
import * as Message from "./message.ts";
import { MAX_MESSAGE_SIZE } from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import {
	hasAnnounceCompression,
	hasAnnounceId,
	hasAnnounceOk,
	hasAnnounceRestart,
	hasExcludeHop,
	hasRouteCost,
	hasRoutes,
	hasSynced,
	Version,
} from "./version.ts";

// Pre-lite-06 inner status values, carried inside the single ANNOUNCE_BROADCAST body.
const STATUS_ENDED = 0;
const STATUS_ACTIVE = 1;
// The lite-05 draft's explicit `restart` status: a metadata update.
const STATUS_UPDATE = 2;

// lite-06 announce message types: an outer discriminator carried before the length
// prefix, so each announcement is an independently-typed, length-delimited message
// (mirroring SUBSCRIBE_START/END/DROP on the subscribe stream).
const ANNOUNCE_START = 0;
const ANNOUNCE_END = 1;
const ANNOUNCE_UPDATE = 2;
// lite-07: another publisher instance replaces the advertisement.
const ANNOUNCE_RESTART = 3;
const SYNCED = 4;
// lite-07: the route layer.
const ROUTE_START = 6;
const ROUTE_UPDATE = 7;
const ROUTE_END = 8;
const ANNOUNCE_FLOOR = 9;
const ROUTE_HOLD = 11;
const DEAD = 15;
// lite-07: an edit of the prefixes the sender wants, and the marker answering one.
const ANNOUNCE_REQUEST = 16;
const ANNOUNCE_OK = 17;

// DEAD kinds.
const DEAD_CLOSED = 0n;
const DEAD_DRAIN = 1n;

/** The largest lite-07 varint. No origin advertises it as a Revision: it leaves room for the
 * floor's offset. */
export const ROUTE_MAX = 2n ** 64n - 1n;

/** Lite-07: copy the first `keep` path segments of the live announcement `distance` back from
 * the stream's next Announce ID, where 1 is the latest ANNOUNCE_START. Resolved by
 * {@link AnnounceHistory}. */
export type Base = { distance: bigint; keep: number };

/** One node a lite-07 route crossed: its Node ID, and the session it learned the route on, an
 * opaque per-node counter that is 0 for the publisher's own hop. */
export interface RouteHop {
	node: bigint;
	session: bigint;
}

/** How a lite-07 publisher reaches a publisher node: the `path` it crossed, the publisher first and
 * the sender last, whether a node on it sent GOAWAY, the lowest link priority on it, the sender's
 * metric to the publisher, and whether it may only travel away from an upstream. Mirrors
 * `route::path::Offer` in rs/moq-net. */
export interface Offer {
	path: RouteHop[];
	draining: boolean;
	priority: number;
	metric: bigint;
	downOnly: boolean;
}

/** A Route Offer this node refuses: the path it still names, which a later DEAD may match, and why. */
export interface Unusable {
	refused: Refusal;
	path: RouteHop[];
}

/** News on DEAD about the routes the sender offered. `closed`: the session `hop` names closed, at
 * the node `far` at its other end. The next `moved` messages on the stream are ROUTE_UPDATEs, one
 * per route on the stream through that hop that continues on another path, and the receiver
 * applies the DEAD and all of them as one. The rest end if `gone`, and are held if not, unless the
 * receiver is the node just before the sender on the route's path. `drain`: `node` sent GOAWAY, so
 * every path through it ranks as draining. */
export type Dead =
	| { kind: "closed"; hop: RouteHop; far: bigint; gone: boolean; moved: bigint }
	| { kind: "drain"; node: bigint };

/** The most Hops a route may carry. One that arrives with this many is unusable. */
export const ROUTE_MAX_HOPS = 32;

// Route Offer flags.
const DOWN_ONLY = 0x1;
const DRAINING = 0x2;

/**
 * A message on the Announce Stream.
 *
 * Lite-07 carries everything one side says about routing on its one unidirectional stream: the
 * edits of the prefixes it wants (`request`), and the markers (`ok`), routes, and claims answering
 * the prefixes its peer wants.
 *
 * On lite-06+ these are independently-typed messages (`ANNOUNCE_START`, `ANNOUNCE_END`,
 * `ANNOUNCE_UPDATE`, and on lite-07 `ANNOUNCE_RESTART`, `SYNCED`, the ROUTE messages, and
 * `ANNOUNCE_FLOOR`), each framed as `Type | Length | Body` like the subscribe stream's responses.
 * Each `active` (ANNOUNCE_START) implicitly assigns the next announce id (a per-stream ordinal
 * starting at 0), and on lite-07 each `routeStart` the next Route ID; the others reference those
 * ids. Older versions send a single ANNOUNCE_BROADCAST message that retracts by path (`ended`).
 *
 * Lite-07 names the route a claim travels under (`route`) in place of a hop chain, so `hops` is
 * empty there; every earlier version carries the chain and no route.
 */
export type AnnounceBroadcast =
	/** A broadcast is now available, carrying the path suffix, relative to the requested prefix
	 * before lite-07 and to the session from it, and (lite-06+) the cost. An absent
	 * cost encodes as zero; it decodes as `undefined` on a wire with no room for one. Before
	 * lite-07 it carries the hop chain; on lite-07 the Route ID, the instance and epoch, which
	 * change only with a `restart`, and a `pathBase` the suffix follows. */
	| {
			status: "active";
			suffix: Path.Valid;
			route?: bigint;
			instance?: Instance.Valid;
			epoch?: bigint;
			hops?: Hop[];
			cost?: Cost;
			pathBase?: Base;
	  }
	/** Pre-lite-06: a broadcast is no longer available, retracted by path. */
	| { status: "ended"; suffix: Path.Valid }
	/** Lite06+: a broadcast is no longer available, retracted by announce id.
	 * The id is retired; referencing it again is a protocol violation. */
	| { status: "endedId"; id: bigint }
	/** Lite06+: atomically replace the metadata of the announcement with this id: its cost, and
	 * before lite-07 its hop chain. The id stays live. */
	| { status: "update"; id: bigint; hops?: Hop[]; cost?: Cost }
	/** Lite07+: the announcement with this id now serves the instance and epoch given. Another
	 * instance is another stream, so what the receiver resolved under its prefix is stale; a
	 * higher epoch of the same one continues it. The id stays live. */
	| { status: "restart"; id: bigint; instance?: Instance.Valid; epoch?: bigint; cost?: Cost }
	/** An unknown lite-06+ announce type, skipped by length. Does not assign an id. */
	| { status: "skipped" }
	/** Lite07+ SYNCED: the initial set of the prefix the receiver assigned this Prefix ID has been
	 * delivered. */
	| { status: "synced"; prefix: bigint }
	/** Lite07+ ANNOUNCE_REQUEST: the sender edits the prefixes it wants claims for, removing those
	 * with the Prefix IDs in `remove` and then adding each of `add`, which assigns it the next
	 * Prefix ID. */
	| { status: "request"; remove: bigint[]; add: Add[] }
	/** Lite07+ ANNOUNCE_OK: the receiver's last ANNOUNCE_REQUEST takes effect here. */
	| { status: "ok" }
	/** Lite07+ ROUTE_START: the publisher reaches the publisher node `node` along `offer`, covering
	 * the prefix the receiver assigned the Prefix ID `prefix` joined with `suffix`, and assigns the
	 * route the next Route ID. */
	| { status: "routeStart"; node: bigint; prefix: bigint; suffix: Path.Valid; offer: Offer }
	/** Lite07+ ROUTE_UPDATE: the route with this id changed, and is usable. */
	| { status: "routeUpdate"; id: bigint; offer: Offer }
	/** Lite07+ ROUTE_HOLD: the publisher has no usable route with this id, but holds it and its
	 * claims while it recovers. */
	| { status: "routeHold"; id: bigint }
	/** Lite07+ ROUTE_END: the publisher let go of the route with this id and every claim under it. */
	| { status: "routeEnd"; id: bigint }
	/** Lite07+ DEAD: a session or node on the routes the publisher offered is gone or going. */
	| { status: "dead"; dead: Dead }
	/** Lite07+ ANNOUNCE_FLOOR: closes the batch of claims under the route with this id, certifying
	 * the stream's claims for it across the requested prefix as the origin's at `revision`, or
	 * nothing when `revision` is undefined. */
	| { status: "floor"; id: bigint; revision?: bigint }
	/** Lite07+: a route message whose content, which the sender only forwarded, this node refuses.
	 * The receiver withdraws what it names and keeps the session. Decoded only as far as keeps the
	 * stream's ids in step; never sent. */
	| { status: "refused"; message: Refusable; reason: Refusal };

/** A prefix an ANNOUNCE_REQUEST adds, relative to the session, and whether it also wants the
 * hidden claims beneath it. */
export interface Add {
	prefix: Path.Valid;
	hidden: boolean;
}

/** What a refused route message still tells the receiver, so the ids that follow it resolve. */
export type Refusable =
	/** ROUTE_START: assigns the next Route ID to the publisher `node`, covering the prefix with the
	 * Prefix ID `prefix` joined with `suffix`, or no prefix when the suffix itself was refused,
	 * along `path`. */
	| { kind: "routeStart"; node: bigint; prefix: bigint; suffix?: Path.Valid; path: RouteHop[] }
	/** ROUTE_UPDATE of the route with this Route ID, now along `path`. */
	| { kind: "routeUpdate"; id: bigint; path: RouteHop[] }
	/** ANNOUNCE_START under Route ID `route`: assigns the next Announce ID, to `path`, or to no path
	 * when the path itself was refused. */
	| { kind: "start"; route: bigint; path?: { suffix: Path.Valid; pathBase?: Base } }
	/** ANNOUNCE_RESTART of the claim with this Announce ID. */
	| { kind: "change"; id: bigint };

/** Why a node refuses content a peer forwarded: well framed, but nothing it can use. */
export type Refusal = "node" | "flags" | "hops" | "path" | "instance";

// Both wire rules on a hop chain, applied to what we send and to what we receive: a
// chain that revisits a hop looped, so neither forwarding it nor subscribing through it
// is safe, and a receiver must end the session over one. `UNKNOWN_HOP` identifies
// nothing, so any number of hops may be unknown.
//
// `ProtocolViolation` so a receipt takes the session down rather than the one stream,
// matching what `ietf/cluster.ts` throws for the identical rule.
function checkHops(hops: Hop[]) {
	if (hops.length > MAX_HOPS) {
		throw new ProtocolViolation(`hop count ${hops.length} exceeds maximum ${MAX_HOPS}`);
	}

	// MAX_HOPS is 32, so the quadratic scan is cheaper than allocating a set.
	for (let i = 0; i < hops.length; i++) {
		const hop = hops[i];
		if (hop === UNKNOWN_HOP) continue;
		if (hops.indexOf(hop, i + 1) !== -1) {
			throw new ProtocolViolation(`hop ${hop} appears twice in the chain`);
		}
	}
}

async function encodeHops(w: Writer, version: Version, hops: Hop[] = []) {
	checkHops(hops);
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			break;
		case Version.DRAFT_03:
			await w.u53(hops.length);
			break;
		default:
			// Lite-07 names a route instead, and JS never offers to read the Lite-06 Chain.
			if (hasRoutes(version)) {
				if (hops.length > 0) throw new Error("lite-07 claims carry no hop chain");
				break;
			}
			// Lite04+: hop count + individual Hop varints.
			await w.u53(hops.length);
			for (const origin of hops) {
				await w.u62(wireHop(origin, version));
			}
			break;
	}
}

async function decodeHops(r: Reader, version: Version): Promise<Hop[]> {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			return [];
		case Version.DRAFT_03: {
			const count = await r.u53();
			if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
			// Lite03 carries only a hop count, not individual ids, so every entry is
			// the reserved "no identity" id.
			return new Array<Hop>(count).fill(UNKNOWN_HOP);
		}
		default: {
			// Lite04+: hop count + individual Hop varints.
			const count = await r.u53();
			if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
			const hops: Hop[] = [];
			for (let i = 0; i < count; i++) {
				hops.push(toHop(await r.u62()));
			}
			checkHops(hops);
			return hops;
		}
	}
}

// The hop chain of an ANNOUNCE_START or ANNOUNCE_UPDATE. Lite-07 names a route instead, and
// carries a trailing Lite-06 Chain only to a peer that offered to read it, which JS never does,
// so any bytes left over are malformed.
async function decodeChain(r: Reader, version: Version): Promise<{ hops?: Hop[] }> {
	return hasRoutes(version) ? {} : { hops: await decodeHops(r, version) };
}

async function encodePath(w: Writer, version: Version, suffix: Path.Valid, base: Base | undefined) {
	if (hasAnnounceCompression(version)) {
		await w.u62(base?.distance ?? 0n);
		await w.u53(base?.keep ?? 0);
	} else if (base) {
		throw new Error("announce compression not supported for this version");
	}
	await w.string(Path.encode(suffix));
}

async function decodePath(r: Reader, version: Version): Promise<{ suffix: Path.Valid; pathBase?: Base }> {
	if (!hasAnnounceCompression(version)) return { suffix: Path.decode(await r.string()) };
	const distance = await r.u62();
	const keep = await r.u53();
	// A distance of 0 names nothing, so a keep without a base is a violation.
	if (distance === 0n && keep !== 0) throw new ProtocolViolation("announce keep without a base");
	const suffix = Path.decode(await r.string());
	return distance === 0n ? { suffix } : { suffix, pathBase: { distance, keep } };
}

// Lite07 carries one static cost. Lite06 retains its second field at the ceiling;
// older versions carry neither. Costs saturate at 2^62-1 on every version, even where lite-07's
// varints could carry more, so a cost always forwards to a peer on an older version.
const MAX_COST = 2n ** 62n - 1n;
const saturate = (v: bigint) => (v > MAX_COST ? MAX_COST : v);

async function encodeRouteCost(w: Writer, version: Version, cost: Cost | undefined) {
	if (!hasRouteCost(version)) return;
	await w.u62(saturate(cost ?? 0n));
	if (version === Version.DRAFT_06) await w.u62(MAX_COST);
}

async function decodeRouteCost(r: Reader, version: Version): Promise<Cost | undefined> {
	if (!hasRouteCost(version)) return undefined;
	const cost = saturate(await r.u62());
	if (version === Version.DRAFT_06) await r.u62();
	return cost;
}

/** Encode a Route Offer: the path, then flags, priority, and metric. No node offers an empty
 * path or one past {@link ROUTE_MAX_HOPS}, nor names Node ID 0 in one. */
async function encodeOffer(w: Writer, offer: Offer) {
	if (offer.path.length === 0 || offer.path.length > ROUTE_MAX_HOPS || offer.path.some((hop) => hop.node === 0n))
		throw new Error("no node offers that path");
	await w.u62(BigInt(offer.path.length));
	for (const hop of offer.path) {
		await w.u62(hop.node);
		await w.u62(hop.session);
	}
	await w.u8((offer.downOnly ? DOWN_ONLY : 0) | (offer.draining ? DRAINING : 0));
	await w.u8(toWire(offer.priority));
	await w.u62(offer.metric);
}

/** Decode a Route Offer. Its path comes first, so a node refusing the rest still learns the path a
 * later DEAD may name. An empty path is the sender's own error, since it names itself last. Hops
 * past the limit, a hop naming Node ID 0, and a flag this node does not know refuse the offer: a
 * newer flag may add a field after the ones this node reads, so a refusal skips the rest of the
 * body. Every hop a path counts is read, but of a path past the limit only those up to one past
 * it are kept: a sender within the limit never sends one, and the whole would hold a body's
 * worth for each route. Mirrors rs/moq-net. */
async function decodeOffer(r: Reader): Promise<Offer | Unusable> {
	const count = await r.u62();
	if (count === 0n) throw new ProtocolViolation("a route offer with no hops");
	const path: RouteHop[] = [];
	for (let i = 0n; i < count; i++) {
		const hop = { node: await r.u62(), session: await r.u62() };
		if (path.length <= ROUTE_MAX_HOPS) path.push(hop);
	}
	const refuse = async (refused: Refusal): Promise<Unusable> => {
		await r.readAll();
		return { refused, path };
	};
	if (count > BigInt(ROUTE_MAX_HOPS)) return refuse("hops");
	if (path.some((hop) => hop.node === 0n)) return refuse("node");
	const flags = await r.u8();
	if (flags & ~(DOWN_ONLY | DRAINING)) return refuse("flags");
	const priority = fromWire(await r.u8());
	const metric = await r.u62();
	return { path, draining: (flags & DRAINING) !== 0, priority, metric, downOnly: (flags & DOWN_ONLY) !== 0 };
}

async function encodeDead(w: Writer, dead: Dead) {
	if (dead.kind === "drain") {
		if (dead.node === 0n) throw new Error("Node ID 0 names no node");
		await w.u62(DEAD_DRAIN);
		await w.u62(dead.node);
		return;
	}
	if (dead.hop.node === 0n || dead.far === 0n) throw new Error("Node ID 0 names no node");
	await w.u62(DEAD_CLOSED);
	await w.u62(dead.hop.node);
	await w.u62(dead.hop.session);
	await w.u62(dead.far);
	await w.u8(dead.gone ? 1 : 0);
	await w.u62(dead.moved);
}

async function decodeDead(r: Reader): Promise<Dead> {
	const nonzero = (id: bigint) => {
		if (id === 0n) throw new ProtocolViolation("Node ID 0 names no node");
		return id;
	};
	const kind = await r.u62();
	if (kind === DEAD_DRAIN) return { kind: "drain", node: nonzero(await r.u62()) };
	if (kind !== DEAD_CLOSED) throw new ProtocolViolation(`unknown DEAD kind ${kind}`);
	const hop = { node: nonzero(await r.u62()), session: await r.u62() };
	const far = nonzero(await r.u62());
	const gone = await r.u8();
	if (gone > 1) throw new ProtocolViolation(`DEAD Gone ${gone}`);
	return { kind: "closed", hop, far, gone: gone === 1, moved: await r.u62() };
}

/** Encode an ANNOUNCE_REQUEST body: the Prefix IDs to remove, then the prefixes to add, each with
 * its `Hidden`. */
async function encodeRequest(w: Writer, remove: bigint[], add: Add[]) {
	await w.u53(remove.length);
	for (const id of remove) await w.u62(id);
	await w.u53(add.length);
	for (const { prefix, hidden } of add) {
		await w.string(Path.encode(prefix));
		await w.bool(hidden);
	}
}

/** Decode an ANNOUNCE_REQUEST body. The prefixes are the sender's own, so one that is no path
 * fails the message rather than being refused. Each entry takes bytes, so the body bounds both
 * counts before either loop runs out. */
async function decodeRequest(r: Reader): Promise<AnnounceBroadcast> {
	const remove: bigint[] = [];
	for (let left = await r.u62(); left > 0n; left--) remove.push(await r.u62());
	const add: Add[] = [];
	for (let left = await r.u62(); left > 0n; left--) {
		add.push({ prefix: Path.decode(await r.string()), hidden: await r.bool() });
	}
	return { status: "request", remove, add };
}

/** The bytes the prefixes `add` and the Prefix IDs `remove` take in an ANNOUNCE_REQUEST, beside
 * its two counts. */
function editBytes(add: Iterable<Path.Valid>, remove: Iterable<bigint>, version: Version): number {
	const size = (value: number | bigint) => encodeVarint(value, version).byteLength;
	const encoder = new TextEncoder();
	let bytes = 0;
	for (const id of remove) bytes += size(id);
	for (const prefix of add) {
		const length = encoder.encode(Path.encode(prefix)).byteLength;
		bytes += size(length) + length + 1;
	}
	return bytes;
}

/** Whether the prefixes `set` fit the Adds of one ANNOUNCE_REQUEST, as a subscriber's whole set
 * must, beside the Prefix IDs `remove`. */
export function requestFits(set: Iterable<Path.Valid>, version: Version, remove: readonly bigint[] = []): boolean {
	const add = [...set];
	const counts = encodeVarint(remove.length, version).byteLength + encodeVarint(add.length, version).byteLength;
	return editBytes(add, remove, version) + counts <= MAX_MESSAGE_SIZE;
}

/**
 * The next part of an edit of a subscriber's prefixes that removes `removed` and adds `add`.
 *
 * Both sets fit one ANNOUNCE_REQUEST as Adds, but an edit also names each prefix it removes, so a
 * large swap can pass one request: it then goes in parts, each changing whole groups of a prefix
 * and those at or inside it, which it replaces at once so nothing still wanted goes unrequested in
 * between. The groups that shrink the set go first, so every set between the two fits too.
 * Mirrors `next_edit` in rs/moq-net.
 *
 * @internal
 */
export function editPart(
	removed: { id: bigint; path: Path.Valid }[],
	add: Add[],
	version: Version,
): { remove: bigint[]; add: Add[] } {
	const remove = removed.map(({ id }) => id);
	if (
		requestFits(
			add.map(({ prefix }) => prefix),
			version,
			remove,
		)
	)
		return { remove, add };

	// Each side's prefixes lie inside no other, so a group is a prefix of one side and those of
	// the other at or inside it, keyed by that widest one.
	const adds = new Set(add.map(({ prefix }) => prefix));
	const gones = new Set(removed.map(({ path }) => path));
	const widest = (path: Path.Valid, other: Set<Path.Valid>) => {
		let top: Path.Valid | undefined;
		for (const above of coveringPrefixes(path)) if (other.has(above)) top = above;
		return top ?? path;
	};
	const groups = new Map<Path.Valid, { remove: bigint[]; add: Add[]; gone: Path.Valid[] }>();
	const group = (key: Path.Valid) => {
		let found = groups.get(key);
		if (!found) {
			found = { remove: [], add: [], gone: [] };
			groups.set(key, found);
		}
		return found;
	};
	for (const { id, path } of removed) {
		const found = group(widest(path, adds));
		found.remove.push(id);
		found.gone.push(path);
	}
	for (const added of add) group(widest(added.prefix, gones)).add.push(added);
	const sized = [...groups.values()].map((found) => {
		const paths = found.add.map(({ prefix }) => prefix);
		const cost = editBytes(paths, found.remove, version);
		const delta = editBytes(paths, [], version) - editBytes(found.gone, [], version);
		return { ...found, cost, delta };
	});
	sized.sort((a, b) => a.delta - b.delta);

	// The two counts take at most 8 bytes each. Any one group fits: it removes no more prefixes
	// than one request held beside one add, or adds no more than one request holds beside one
	// remove.
	const part: { remove: bigint[]; add: Add[] } = { remove: [], add: [] };
	let spent = 16;
	for (const found of sized) {
		if (spent + found.cost > MAX_MESSAGE_SIZE && spent > 16) break;
		spent += found.cost;
		part.remove.push(...found.remove);
		part.add.push(...found.add);
	}
	return part;
}

/** Decode a lite-07 suffix: its base, which the sender picked, then the path it forwarded, or
 * undefined for a path this node refuses. */
async function decodeForwardedPath(r: Reader): Promise<{ suffix: Path.Valid; pathBase?: Base } | undefined> {
	const distance = await r.u62();
	const keep = await r.u53();
	// A distance of 0 names nothing, so a keep without a base is a violation.
	if (distance === 0n && keep !== 0) throw new ProtocolViolation("announce keep without a base");
	const text = utf8(await r.read(await r.u53()));
	const suffix = text === undefined ? undefined : Path.tryDecode(text);
	if (suffix === undefined) return undefined;
	return distance === 0n ? { suffix } : { suffix, pathBase: { distance, keep } };
}

/** A lite-07 ANNOUNCE_START: its suffix, Route ID, instance and epoch, and cost. */
async function decodeStart(r: Reader, version: Version): Promise<AnnounceBroadcast> {
	const path = await decodeForwardedPath(r);
	const route = await r.u62();
	const refused = async (reason: Refusal): Promise<AnnounceBroadcast> => {
		await r.readAll();
		return { status: "refused", message: { kind: "start", route, path }, reason };
	};
	if (!path) return refused("path");
	const { instance, epoch } = await decodeForwarded(r);
	if (instance === undefined) return refused("instance");
	return { status: "active", ...path, route, instance, epoch, cost: await decodeRouteCost(r, version) };
}

/** A lite-07 ANNOUNCE_RESTART: its Announce ID, instance and epoch, and cost. */
async function decodeRestart(r: Reader, version: Version): Promise<AnnounceBroadcast> {
	const id = await r.u62();
	const { instance, epoch } = await decodeForwarded(r);
	if (instance === undefined) {
		await r.readAll();
		return { status: "refused", message: { kind: "change", id }, reason: "instance" };
	}
	return { status: "restart", id, instance, epoch, cost: await decodeRouteCost(r, version) };
}

// lite-06 message body (no discriminator; the type is carried outside the length prefix).
async function encodeAnnounce06Body(w: Writer, msg: AnnounceBroadcast, version: Version) {
	switch (msg.status) {
		case "active":
			await encodePath(w, version, msg.suffix, msg.pathBase);
			if (hasRoutes(version)) {
				if (msg.route === undefined) throw new Error("a lite-07 claim names its route");
				await w.u62(msg.route);
			} else if (msg.route !== undefined) {
				throw new Error("routes not supported for this version");
			}
			await encodeRoute(w, version, msg.instance, msg.epoch);
			await encodeHops(w, version, msg.hops);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "endedId":
			await w.u62(msg.id);
			break;
		case "update":
			await w.u62(msg.id);
			await encodeHops(w, version, msg.hops);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "restart":
			await w.u62(msg.id);
			await encodeRoute(w, version, msg.instance, msg.epoch);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "routeStart":
			if (msg.node === 0n) throw new Error("Node ID 0 names no origin");
			await w.u62(msg.node);
			await w.u62(msg.prefix);
			await w.string(Path.encode(msg.suffix));
			await encodeOffer(w, msg.offer);
			break;
		case "routeUpdate":
			await w.u62(msg.id);
			await encodeOffer(w, msg.offer);
			break;
		case "dead":
			await encodeDead(w, msg.dead);
			break;
		case "routeHold":
		case "routeEnd":
			await w.u62(msg.id);
			break;
		case "floor":
			await w.u62(msg.id);
			// No origin advertises the last revision, so it never needs the offset.
			if (msg.revision !== undefined && msg.revision >= ROUTE_MAX) throw new Error("revision out of range");
			await w.u62(msg.revision === undefined ? 0n : msg.revision + 1n);
			break;
		case "synced":
			await w.u62(msg.prefix);
			break;
		case "request":
			await encodeRequest(w, msg.remove, msg.add);
			break;
		case "ok":
			break;
		case "ended":
			// The pre-lite-06 path-form retraction has no place on lite-06.
			throw new Error("ended-by-path not supported for this version");
		case "skipped":
		case "refused":
			throw new Error("decode-only announce type cannot be encoded");
	}
}

// lite-06 outer message type for a given announcement.
function announce06Type(msg: AnnounceBroadcast, version: Version): number {
	switch (msg.status) {
		case "active":
			return ANNOUNCE_START;
		case "endedId":
			return ANNOUNCE_END;
		case "update":
			return ANNOUNCE_UPDATE;
		case "restart":
			// Older versions send an end and a start instead.
			if (!hasAnnounceRestart(version)) throw new Error("announce restart not supported for this version");
			return ANNOUNCE_RESTART;
		case "synced":
			if (!hasSynced(version)) throw new Error("synced not supported for this version");
			return SYNCED;
		case "request":
		case "ok":
		case "routeStart":
		case "routeUpdate":
		case "routeHold":
		case "routeEnd":
		case "dead":
		case "floor":
			if (!hasRoutes(version)) throw new Error("routes not supported for this version");
			return {
				request: ANNOUNCE_REQUEST,
				ok: ANNOUNCE_OK,
				routeStart: ROUTE_START,
				routeUpdate: ROUTE_UPDATE,
				routeHold: ROUTE_HOLD,
				routeEnd: ROUTE_END,
				dead: DEAD,
				floor: ANNOUNCE_FLOOR,
			}[msg.status];
		case "ended":
			throw new Error("ended-by-path not supported for this version");
		case "skipped":
		case "refused":
			throw new Error("decode-only announce type cannot be encoded");
	}
}

async function decodeAnnounce06Body(r: Reader, typ: number, version: Version): Promise<AnnounceBroadcast> {
	switch (typ) {
		case ANNOUNCE_START: {
			// Lite-07's claims carry what the sender forwarded, which a receiver may refuse.
			if (hasRoutes(version)) return decodeStart(r, version);
			const path = await decodePath(r, version);
			const route = hasRoutes(version) ? { route: await r.u62() } : {};
			const served = await decodeRoute(r, version);
			const hops = await decodeChain(r, version);
			return { status: "active", ...path, ...route, ...served, ...hops, cost: await decodeRouteCost(r, version) };
		}
		case SYNCED:
			if (!hasSynced(version)) break;
			return { status: "synced", prefix: await r.u62() };
		case ANNOUNCE_REQUEST:
			if (!hasRoutes(version)) break;
			return decodeRequest(r);
		case ANNOUNCE_OK:
			if (!hasRoutes(version)) break;
			return { status: "ok" };
		case ANNOUNCE_END:
			return { status: "endedId", id: await r.u62() };
		case ANNOUNCE_UPDATE: {
			const id = await r.u62();
			const hops = await decodeChain(r, version);
			return { status: "update", id, ...hops, cost: await decodeRouteCost(r, version) };
		}
		case ANNOUNCE_RESTART: {
			// Unknown before lite-07: skipped below like any other.
			if (!hasAnnounceRestart(version)) break;
			if (hasRoutes(version)) return decodeRestart(r, version);
			const id = await r.u62();
			const served = await decodeRoute(r, version);
			return { status: "restart", id, ...served, cost: await decodeRouteCost(r, version) };
		}
		case ROUTE_START: {
			if (!hasRoutes(version)) break;
			const node = await r.u62();
			const prefix = await r.u62();
			// The suffix is sized, so the offer's path reads even when it is refused.
			const text = utf8(await r.read(await r.u53()));
			const suffix = text === undefined ? undefined : Path.tryDecode(text);
			const offer = await decodeOffer(r);
			if (node !== 0n && suffix !== undefined && !("refused" in offer))
				return { status: "routeStart", node, prefix, suffix, offer };
			const reason = node === 0n ? "node" : suffix === undefined ? "path" : (offer as Unusable).refused;
			return {
				status: "refused",
				message: { kind: "routeStart", node, prefix, suffix, path: offer.path },
				reason,
			};
		}
		case ROUTE_UPDATE: {
			if (!hasRoutes(version)) break;
			const id = await r.u62();
			const offer = await decodeOffer(r);
			if ("refused" in offer)
				return {
					status: "refused",
					message: { kind: "routeUpdate", id, path: offer.path },
					reason: offer.refused,
				};
			return { status: "routeUpdate", id, offer };
		}
		case DEAD:
			if (!hasRoutes(version)) break;
			return { status: "dead", dead: await decodeDead(r) };
		case ROUTE_HOLD:
			if (!hasRoutes(version)) break;
			return { status: "routeHold", id: await r.u62() };
		case ROUTE_END:
			if (!hasRoutes(version)) break;
			return { status: "routeEnd", id: await r.u62() };
		case ANNOUNCE_FLOOR: {
			if (!hasRoutes(version)) break;
			const id = await r.u62();
			const revision = await r.u62();
			return revision === 0n ? { status: "floor", id } : { status: "floor", id, revision: revision - 1n };
		}
	}
	// Skip the length-prefixed body so an earlier Lite06 build negotiating
	// the same ALPN does not kill the announce stream.
	await r.readAll();
	return { status: "skipped" };
}

// Pre-lite-06 single ANNOUNCE_BROADCAST body: an inner status byte, then path + hops.
async function encodeLegacyBody(w: Writer, msg: AnnounceBroadcast, version: Version) {
	switch (msg.status) {
		case "active":
			if (msg.pathBase) throw new Error("announce compression not supported for this version");
			if (msg.route !== undefined) throw new Error("routes not supported for this version");
			await w.u8(STATUS_ACTIVE);
			await w.string(Path.encode(msg.suffix));
			await encodeHops(w, version, msg.hops);
			break;
		case "ended":
			await w.u8(STATUS_ENDED);
			await w.string(Path.encode(msg.suffix));
			await encodeHops(w, version, []);
			break;
		default:
			// The id-referencing forms only exist on lite-06+.
			throw new Error("announce ids not supported for this version");
	}
}

async function decodeLegacyBody(r: Reader, version: Version): Promise<AnnounceBroadcast> {
	const status = await r.u8();
	// On lite-05 an update travels as a duplicate `active`, but the explicit update status
	// is accepted on decode and treated the same. Older versions never defined it.
	const active = status === STATUS_ACTIVE || (status === STATUS_UPDATE && hasAnnounceOk(version));
	if (status !== STATUS_ENDED && !active) {
		throw new Error("invalid announce status");
	}
	const suffix = Path.decode(await r.string());
	const hops = await decodeHops(r, version);
	return active ? { status: "active", suffix, hops } : { status: "ended", suffix };
}

/** Encode one announcement, including its type discriminator (lite-06+) and length prefix. */
export async function encodeAnnounceBroadcast(w: Writer, msg: AnnounceBroadcast, version: Version): Promise<void> {
	if (hasAnnounceId(version)) {
		// lite-06+: outer type discriminator, then a size-prefixed body (like the subscribe stream).
		return Message.encode(w, (w) => encodeAnnounce06Body(w, msg, version), { id: announce06Type(msg, version) });
	}
	return Message.encode(w, (w) => encodeLegacyBody(w, msg, version));
}

/** Decode one announcement, including its type discriminator (lite-06+) and length prefix. */
export async function decodeAnnounceBroadcast(r: Reader, version: Version): Promise<AnnounceBroadcast> {
	if (hasAnnounceId(version)) {
		const typ = await r.u53();
		return Message.decode(r, (r) => decodeAnnounce06Body(r, typ, version));
	}
	return Message.decode(r, (r) => decodeLegacyBody(r, version));
}

/** Like {@link decodeAnnounceBroadcast} but resolves `undefined` on a clean FIN. */
export async function decodeAnnounceBroadcastMaybe(
	r: Reader,
	version: Version,
): Promise<AnnounceBroadcast | undefined> {
	if (hasAnnounceId(version)) {
		if (await r.done()) return undefined;
		const typ = await r.u53();
		return Message.decode(r, (r) => decodeAnnounce06Body(r, typ, version));
	}
	return Message.decodeMaybe(r, (r) => decodeLegacyBody(r, version));
}

/**
 * The live announcements on one lite-06+ announce stream, by Announce ID: what ANNOUNCE_END,
 * ANNOUNCE_UPDATE, and ANNOUNCE_RESTART reference, and what a lite-07 base copies from. Every
 * violation throws {@link ProtocolViolation}.
 */
export class AnnounceHistory {
	#next = 0n;
	#live = new Map<bigint, Path.Valid>();
	// The live ids whose path was refused, which a base names only to be refused too.
	#refused = new Set<bigint>();

	/** The Announce ID the next ANNOUNCE_START is assigned. */
	get next(): bigint {
		return this.#next;
	}

	/** An ANNOUNCE_START: assign it the next id, and resolve its suffix, or undefined when that
	 * suffix, or the base it names, is no path this node can hold. */
	start(msg: { suffix: Path.Valid; pathBase?: Base } | undefined): Path.Valid | undefined {
		const suffix = msg && this.#resolve(msg);
		const id = this.#next++;
		if (suffix === undefined) this.#refused.add(id);
		else this.#live.set(id, suffix);
		return suffix;
	}

	#resolve(msg: { suffix: Path.Valid; pathBase?: Base }): Path.Valid | undefined {
		const suffix = msg.suffix;
		if (!msg.pathBase) return suffix;
		const id = this.#next - msg.pathBase.distance;
		if (this.#refused.has(id)) return undefined;
		const base = this.#live.get(id);
		if (base === undefined) throw new ProtocolViolation(`announce base ${msg.pathBase.distance} is not live`);
		const head = Path.parts(base);
		const keep = msg.pathBase.keep;
		if (keep > head.length) throw new ProtocolViolation(`announce keeps ${keep} of ${head.length} segments`);
		// Joined as text before parsing: a kept segment may end in the `\` of an escaped `/`. A path
		// that does not resolve is refused as content, as rs/moq-net does.
		const kept = head.slice(0, keep);
		if (suffix !== Path.empty()) kept.push(suffix);
		return Path.tryDecode(kept.join("/"));
	}

	/** The suffix of the live announcement `id`, which an update or restart references. */
	get(id: bigint): Path.Valid {
		const live = this.#live.get(id);
		if (live === undefined) throw new ProtocolViolation(`unknown announce id: ${id}`);
		return live;
	}

	/** Assign the next id to `path` with no ANNOUNCE_START, as an edit does for a claim it presents
	 * at a prefix below its own. */
	insert(path: Path.Valid): bigint {
		const id = this.#next++;
		this.#live.set(id, path);
		return id;
	}

	/** An ANNOUNCE_END: retire the id, returning its suffix, or undefined if it was refused. */
	end(id: bigint): Path.Valid | undefined {
		if (this.#refused.delete(id)) return undefined;
		const live = this.get(id);
		this.#live.delete(id);
		return live;
	}
}

/**
 * ANNOUNCE_REQUEST before lite-07: sent by the subscriber to request ANNOUNCE_BROADCAST messages
 * for a path prefix, opening a bidirectional Announce Stream. Renamed from `AnnounceInterest` in
 * lite-05. Lite-07 edits a set of prefixes on its one stream instead (the `request` message).
 */
export class AnnounceRequest {
	prefix: Path.Valid;
	/** Lite04/05 only: the Hop id of the peer asking for announces, which the publisher
	 * uses to skip announces that already passed through it. {@link UNKNOWN_HOP} means
	 * "no exclusion". Not on the wire elsewhere, so a value set here is ignored when
	 * encoding for another version and decodes as unknown. */
	excludeHop: Hop;

	constructor(prefix: Path.Valid, excludeHop: Hop = UNKNOWN_HOP) {
		this.prefix = prefix;
		this.excludeHop = excludeHop;
	}

	static #guard(version: Version) {
		if (hasRoutes(version)) throw new Error("announce request streams not supported for this version");
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.prefix));
		if (hasExcludeHop(version)) {
			await w.u62(wireHop(this.excludeHop, version));
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceRequest> {
		const prefix = Path.decode(await r.string());
		const excludeHop = hasExcludeHop(version) ? toHop(await r.u62()) : UNKNOWN_HOP;
		return new AnnounceRequest(prefix, excludeHop);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		AnnounceRequest.#guard(version);
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceRequest> {
		AnnounceRequest.#guard(version);
		return Message.decode(r, (r) => AnnounceRequest.#decode(r, version));
	}
}

/// Sent after setup to communicate the initially announced paths.
///
/// Used by Draft01/Draft02 only. Draft03+ uses individual Announce messages instead.
export class AnnounceInit {
	suffixes: Path.Valid[];

	constructor(paths: Path.Valid[]) {
		this.suffixes = paths;
	}

	static #guard(version: Version) {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				throw new Error("announce init not supported for this version");
		}
	}

	async #encode(w: Writer) {
		await w.u53(this.suffixes.length);
		for (const path of this.suffixes) {
			await w.string(Path.encode(path));
		}
	}

	static async #decode(r: Reader, budget: number): Promise<AnnounceInit> {
		const count = await r.u53();
		// Refused before decoding any, so the set grows no larger than what may be announced.
		if (count > budget) throw new Error(`announce init lists ${count} broadcasts with ${budget} slots left`);
		const suffixes: Path.Valid[] = [];
		for (let i = 0; i < count; i++) {
			suffixes.push(Path.decode(await r.string()));
		}
		return new AnnounceInit(suffixes);
	}

	// The whole initial set in one message, capped like Rust's so it fits one storage segment
	// (1 MiB) with its type and size prefixes. A larger set is refused on both ends.
	static readonly MAX_SIZE = SEGMENT_BYTES - 16;

	async encode(w: Writer, version: Version): Promise<void> {
		AnnounceInit.#guard(version);
		return Message.encode(w, this.#encode.bind(this), { max: AnnounceInit.MAX_SIZE });
	}

	/** Decode the initial set, refusing one that lists more than `budget` broadcasts. */
	static async decode(r: Reader, version: Version, budget: number): Promise<AnnounceInit> {
		AnnounceInit.#guard(version);
		return Message.decode(r, (r) => AnnounceInit.#decode(r, budget), AnnounceInit.MAX_SIZE);
	}
}

/// Sent by the publisher as the first message on an announce stream, before any
/// individual Announce messages. Lite05/06 only; the successor to AnnounceInit. Lite-07 answers
/// on its one stream instead (the `ok` message).
///
/// `origin` is the responder's Hop ID, which the subscriber stamps onto each
/// announce's hop chain (the publisher no longer stamps itself), or the reserved
/// {@link UNKNOWN_HOP} when the responder has no identity to give. `active` is the
/// initial start count.
export class AnnounceOk {
	hop: Hop;
	active: number;

	constructor(hop: Hop, active: number) {
		this.hop = hop;
		this.active = active;
	}

	static #guard(version: Version) {
		if (!hasAnnounceOk(version) || hasRoutes(version)) {
			throw new Error("announce ok not supported for this version");
		}
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(wireHop(this.hop, version));
		await w.u53(this.active);
	}

	static async #decode(r: Reader): Promise<AnnounceOk> {
		// The draft reserves 0 for "unknown": the responder was never assigned an id, or
		// withholds it to obscure its routing. It names nobody, so callers must not stamp
		// it onto a hop chain, but it is a legal message and not grounds to drop the stream.
		const origin = toHop(await r.u62());
		return new AnnounceOk(origin, await r.u53());
	}

	async encode(w: Writer, version: Version): Promise<void> {
		AnnounceOk.#guard(version);
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceOk> {
		AnnounceOk.#guard(version);
		return Message.decode(r, AnnounceOk.#decode);
	}
}

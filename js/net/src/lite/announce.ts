import { ProtocolViolation } from "../error.ts";
import { type Cost, type Hop, HopSchema, MAX_HOPS, UNKNOWN_HOP, wireHop } from "../hop.ts";
import type * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { SEGMENT_BYTES } from "../util/limits.ts";
import { decodeForwarded, decodeRoute, encodeRoute, utf8 } from "./instance.ts";
import * as Message from "./message.ts";
import {
	hasAnnounceCompression,
	hasAnnounceId,
	hasAnnounceOk,
	hasAnnounceRestart,
	hasExcludeHop,
	hasHidden,
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
// lite-07: the route layer. ROUTE_END also travels back as the echo.
const ROUTE_START = 6;
const ROUTE_UPDATE = 7;
const ROUTE_END = 8;
const ANNOUNCE_FLOOR = 9;
const ROUTE_REQUEST = 10;
const ROUTE_HOLD = 11;

/**
 * The largest lite-07 varint. No origin advertises it as a Seqno or Revision: it leaves room for
 * the successor a ROUTE_REQUEST names and for the floor's offset.
 */
export const ROUTE_MAX = 2n ** 64n - 1n;

/** Lite-07: copy the first `keep` path segments of the live announcement `distance` back from
 * the stream's next Announce ID, where 1 is the latest ANNOUNCE_START. Resolved by
 * {@link AnnounceHistory}. */
export type Base = { distance: bigint; keep: number };

/** How a lite-07 publisher reaches an origin node: the origin's seqno, the publisher's metric to it,
 * whether it may only travel away from an upstream, its informational RTT to it in milliseconds,
 * `undefined` when unmeasured, and the Hop ID of every node it crossed, the origin first. */
export interface Advert {
	seqno: bigint;
	metric: bigint;
	downOnly: boolean;
	rtt: bigint | undefined;
	hops: bigint[];
}

/** The most Hops a route may carry. One that arrives with this many is unusable. */
export const ROUTE_MAX_HOPS = 32;

// ROUTE_START and ROUTE_UPDATE flags.
const DOWN_ONLY = 0x1;
const RTT = 0x2;

/**
 * A message on the publisher's side of the Announce Stream.
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
	/** A broadcast is now available, carrying the path suffix, and (lite-06+) the cost. An absent
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
	/** Lite07+: the initial set has been delivered. */
	| { status: "synced" }
	/** Lite07+ ROUTE_START: the publisher reaches the origin node `node`, and assigns the route
	 * the next Route ID. */
	| { status: "routeStart"; node: bigint; advert: Advert }
	/** Lite07+ ROUTE_UPDATE: the route with this id changed, and is usable. */
	| { status: "routeUpdate"; id: bigint; advert: Advert }
	/** Lite07+ ROUTE_HOLD: the publisher has no usable route with this id, but holds it and its
	 * claims while it recovers. */
	| { status: "routeHold"; id: bigint }
	/** Lite07+ ROUTE_END: the publisher let go of the route with this id and every claim under it.
	 * The subscriber echoes it back. */
	| { status: "routeEnd"; id: bigint }
	/** Lite07+ ANNOUNCE_FLOOR: closes the batch of claims under the route with this id, certifying
	 * the stream's claims for it across the requested prefix as the origin's at `revision`, or
	 * nothing when `revision` is undefined. */
	| { status: "floor"; id: bigint; revision?: bigint }
	/** Lite07+: a route message whose content, which the sender only forwarded, this node refuses.
	 * The receiver withdraws what it names and keeps the session. Decoded only as far as keeps the
	 * stream's ids in step; never sent. */
	| { status: "refused"; message: Refusable; reason: Refusal };

/** What a refused route message still tells the receiver, so the ids that follow it resolve. */
export type Refusable =
	/** ROUTE_START: assigns the next Route ID to the origin node `node`. */
	| { kind: "routeStart"; node: bigint }
	/** ROUTE_UPDATE of the route with this Route ID. */
	| { kind: "routeUpdate"; id: bigint }
	/** ANNOUNCE_START under Route ID `route`: assigns the next Announce ID, to `path`, or to no path
	 * when the path itself was refused. */
	| { kind: "start"; route: bigint; path?: { suffix: Path.Valid; pathBase?: Base } }
	/** ANNOUNCE_RESTART of the claim with this Announce ID. */
	| { kind: "change"; id: bigint };

/** Why a node refuses content a peer forwarded: well framed, but nothing it can use. */
export type Refusal = "node" | "seqno" | "flags" | "hops" | "path" | "instance";

/**
 * A message the subscriber sends on a lite-07 Announce Stream after its ANNOUNCE_REQUEST.
 *
 * `request` (ROUTE_REQUEST) asks for a route to `node`, as the publisher named it, at least as new
 * as `seqno`. `echo` is the ROUTE_END that retired Route ID `id`, sent back once the subscriber
 * dropped the route.
 */
export type AnnounceReply = { kind: "request"; node: bigint; seqno: bigint } | { kind: "echo"; id: bigint };

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
				hops.push(HopSchema.parse(await r.u62()));
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

/** Encode a seqno, metric, flags, RTT, and hops. No origin advertises the last seqno, and no
 * route carries more than {@link ROUTE_MAX_HOPS}. */
async function encodeAdvert(w: Writer, advert: Advert) {
	if (advert.seqno >= ROUTE_MAX) throw new Error("a route never advertises the last seqno");
	if (advert.hops.length > ROUTE_MAX_HOPS) throw new Error("a route crossed too many nodes");
	await w.u62(advert.seqno);
	await w.u62(advert.metric);
	await w.u8((advert.downOnly ? DOWN_ONLY : 0) | (advert.rtt === undefined ? 0 : RTT));
	if (advert.rtt !== undefined) await w.u62(advert.rtt);
	await w.u62(BigInt(advert.hops.length));
	for (const hop of advert.hops) await w.u62(hop);
}

/** Decode an advertisement, refusing one whose seqno no origin advertises, whose flags this node
 * does not know, or that crossed too many hops. The rest of the body is skipped, since an unknown
 * flag may announce a field ahead of the hops. Mirrors rs/moq-net. */
async function decodeAdvert(r: Reader): Promise<Advert | Refusal> {
	const refuse = async (reason: Refusal) => {
		await r.readAll();
		return reason;
	};
	const seqno = await r.u62();
	if (seqno === ROUTE_MAX) return refuse("seqno");
	const metric = await r.u62();
	const flags = await r.u8();
	if (flags & ~(DOWN_ONLY | RTT)) return refuse("flags");
	const rtt = flags & RTT ? await r.u62() : undefined;
	const count = await r.u62();
	if (count > BigInt(ROUTE_MAX_HOPS)) return refuse("hops");
	const hops: bigint[] = [];
	for (let i = 0n; i < count; i++) hops.push(await r.u62());
	return { seqno, metric, downOnly: (flags & DOWN_ONLY) !== 0, rtt, hops };
}

/** Refuse a route to the reserved Node ID 0, skipping the rest of its body. */
async function refuseNode(r: Reader): Promise<Refusal> {
	await r.readAll();
	return "node";
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
			await encodeAdvert(w, msg.advert);
			break;
		case "routeUpdate":
			await w.u62(msg.id);
			await encodeAdvert(w, msg.advert);
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
		case "routeStart":
		case "routeUpdate":
		case "routeHold":
		case "routeEnd":
		case "floor":
			if (!hasRoutes(version)) throw new Error("routes not supported for this version");
			return {
				routeStart: ROUTE_START,
				routeUpdate: ROUTE_UPDATE,
				routeHold: ROUTE_HOLD,
				routeEnd: ROUTE_END,
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
			return { status: "synced" };
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
			const advert = node === 0n ? await refuseNode(r) : await decodeAdvert(r);
			if (typeof advert === "string")
				return { status: "refused", message: { kind: "routeStart", node }, reason: advert };
			return { status: "routeStart", node, advert };
		}
		case ROUTE_UPDATE: {
			if (!hasRoutes(version)) break;
			const id = await r.u62();
			const advert = await decodeAdvert(r);
			if (typeof advert === "string")
				return { status: "refused", message: { kind: "routeUpdate", id }, reason: advert };
			return { status: "routeUpdate", id, advert };
		}
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

/** Encode one subscriber reply on a lite-07 Announce Stream. */
export async function encodeAnnounceReply(w: Writer, msg: AnnounceReply, version: Version): Promise<void> {
	if (!hasRoutes(version)) throw new Error("routes not supported for this version");
	if (msg.kind === "echo") return Message.encode(w, (w) => w.u62(msg.id), { id: ROUTE_END });
	if (msg.node === 0n) throw new Error("Node ID 0 names no origin");
	return Message.encode(
		w,
		async (w) => {
			await w.u62(msg.node);
			await w.u62(msg.seqno);
		},
		{ id: ROUTE_REQUEST },
	);
}

/** Decode one subscriber reply, or `undefined` on a clean FIN. Any other type is a violation. */
export async function decodeAnnounceReplyMaybe(r: Reader, version: Version): Promise<AnnounceReply | undefined> {
	if (!hasRoutes(version)) throw new Error("routes not supported for this version");
	if (await r.done()) return undefined;
	const typ = await r.u53();
	return Message.decode(r, async (r): Promise<AnnounceReply> => {
		switch (typ) {
			case ROUTE_REQUEST: {
				const node = await r.u62();
				if (node === 0n) throw new ProtocolViolation("Node ID 0 names no origin");
				return { kind: "request", node, seqno: await r.u62() };
			}
			case ROUTE_END:
				return { kind: "echo", id: await r.u62() };
			default:
				throw new ProtocolViolation(`unexpected announce reply type ${typ}`);
		}
	});
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

	/** An ANNOUNCE_END: retire the id, returning its suffix, or undefined if it was refused. */
	end(id: bigint): Path.Valid | undefined {
		if (this.#refused.delete(id)) return undefined;
		const live = this.get(id);
		this.#live.delete(id);
		return live;
	}
}

/**
 * ANNOUNCE_REQUEST: sent by the subscriber to request ANNOUNCE_BROADCAST messages
 * for a path prefix. Renamed from `AnnounceInterest` in lite-05.
 */
export class AnnounceRequest {
	prefix: Path.Valid;
	/** Lite04/05 only: the Hop id of the peer asking for announces, which the publisher
	 * uses to skip announces that already passed through it. {@link UNKNOWN_HOP} means
	 * "no exclusion". Not on the wire elsewhere, so a value set here is ignored when
	 * encoding for another version and decodes as unknown. */
	excludeHop: Hop;
	/** Lite07+: also announce routes with a `.`-prefixed segment below the prefix. Not on
	 * the wire earlier, so a value set here is ignored when encoding for an older version
	 * and decodes as false. */
	hidden: boolean;

	constructor(prefix: Path.Valid, excludeHop: Hop = UNKNOWN_HOP, hidden = false) {
		this.prefix = prefix;
		this.excludeHop = excludeHop;
		this.hidden = hidden;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.prefix));
		if (hasExcludeHop(version)) {
			await w.u62(wireHop(this.excludeHop, version));
		}
		if (hasHidden(version)) {
			await w.bool(this.hidden);
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceRequest> {
		const prefix = Path.decode(await r.string());
		const excludeHop = hasExcludeHop(version) ? HopSchema.parse(await r.u62()) : UNKNOWN_HOP;
		const hidden = hasHidden(version) ? await r.bool() : false;
		return new AnnounceRequest(prefix, excludeHop, hidden);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceRequest> {
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
/// individual Announce messages. Lite05+ only; the successor to AnnounceInit.
///
/// `origin` is the responder's Hop ID, which the subscriber stamps onto each
/// announce's hop chain (the publisher no longer stamps itself), or the reserved
/// {@link UNKNOWN_HOP} when the responder has no identity to give. Lite05/06
/// carry `active`, the initial start count. Lite-07's is empty: claims name their route
/// instead of a hop chain, and SYNCED marks the initial set, so both decode as unknown.
export class AnnounceOk {
	hop: Hop;
	active: number;

	constructor(hop: Hop, active: number) {
		this.hop = hop;
		this.active = active;
	}

	static #guard(version: Version) {
		if (!hasAnnounceOk(version)) {
			throw new Error("announce ok not supported for this version");
		}
	}

	async #encode(w: Writer, version: Version) {
		if (hasRoutes(version)) return;
		await w.u62(wireHop(this.hop, version));
		if (!hasSynced(version)) await w.u53(this.active);
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceOk> {
		if (hasRoutes(version)) return new AnnounceOk(UNKNOWN_HOP, 0);
		// The draft reserves 0 for "unknown": the responder was never assigned an id, or
		// withholds it to obscure its routing. It names nobody, so callers must not stamp
		// it onto a hop chain, but it is a legal message and not grounds to drop the stream.
		const origin = HopSchema.parse(await r.u62());
		const active = hasSynced(version) ? 0 : await r.u53();
		return new AnnounceOk(origin, active);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		AnnounceOk.#guard(version);
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceOk> {
		AnnounceOk.#guard(version);
		return Message.decode(r, (r) => AnnounceOk.#decode(r, version));
	}
}

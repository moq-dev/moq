import { ProtocolViolation } from "../error.ts";
import { type Cost, type Hop, MAX_HOPS, toHop, UNKNOWN_HOP, wireHop } from "../hop.ts";
import type * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import { encodeVarint, type Reader, type Writer } from "../stream.ts";
import { SEGMENT_BYTES } from "../util/limits.ts";
import { decodeRoute, encodeRoute } from "./instance.ts";
import * as Message from "./message.ts";
import { MAX_MESSAGE_SIZE } from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import {
	hasAnnounceCompression,
	hasAnnounceId,
	hasAnnounceOk,
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
// lite-07: what only two nodes that both declared a Node ID exchange. An endpoint declares none,
// so receiving one is a protocol violation.
const ANNOUNCE_FLOOR = 3;
const SYNCED = 4;
// lite-07: an edit of the prefixes the sender wants, and the marker answering one.
const ANNOUNCE_REQUEST = 5;
const ANNOUNCE_OK = 6;
const ROUTE = 7;
const ROUTE_END = 8;
const DEAD = 9;
const DRAIN = 10;

/** Lite-07: copy the first `keep` path segments of the live announcement `distance` back from
 * the stream's next Announce ID, where 1 is the latest ANNOUNCE_START. Resolved by
 * {@link AnnounceHistory}. */
export type Base = { distance: bigint; keep: number };

/** The most nodes a lite-07 claim's chain may name: one that would reach 32 is never taken. */
export const CHAIN_MAX = 31;

// Claim flags.
const UNAVAILABLE = 0x1;
const DRAINING = 0x2;
const MARKED = 0x4;

/**
 * What a lite-07 claim says beside its path and instance. Mirrors `route::relay::Claim` in
 * rs/moq-net.
 */
export interface Claim {
	/** Which incarnation of the instance this is. */
	epoch: bigint;
	/** The sender's cost to the claim's publisher. */
	cost: Cost;
	/** How many segments broader the claim is than the path it is presented at. */
	above: bigint;
	/** No route reaches the claim from the node that told it: requests for it fail until one does. */
	unavailable: boolean;
	/** A node on the way to its publisher sent GOAWAY. */
	draining: boolean;
	/** A marked session lies on the way to its publisher, so a relay never tells it over another. */
	marked: boolean;
	/** The lowest link priority on the way to its publisher. */
	priority: number;
	/** The Node IDs that announced the claim again across a scoped grant or an older version. */
	chain: bigint[];
}

/** A claim this origin publishes itself: no cost, at the top priority, announced again by no one. */
export function ownClaim(epoch: bigint, above = 0n): Claim {
	return { epoch, cost: 0n, above, unavailable: false, draining: false, marked: false, priority: 127, chain: [] };
}

/** Whether two claims say the same. */
export function claimsEqual(a: Claim, b: Claim): boolean {
	return (
		a.epoch === b.epoch &&
		a.cost === b.cost &&
		a.above === b.above &&
		a.unavailable === b.unavailable &&
		a.draining === b.draining &&
		a.marked === b.marked &&
		a.priority === b.priority &&
		a.chain.length === b.chain.length &&
		a.chain.every((node, i) => node === b.chain[i])
	);
}

/**
 * A message on the Announce Stream.
 *
 * Lite-07 carries everything one side says about its claims on its one unidirectional stream:
 * the edits of the prefixes it wants (`request`), and the markers (`ok`, `synced`) and claims
 * answering the prefixes its peer wants. Two nodes that both declared a Node ID also exchange
 * routes there, which an endpoint never sends and refuses to receive.
 *
 * On lite-06+ these are independently-typed messages, each framed as `Type | Length | Body`
 * like the subscribe stream's responses. Each ANNOUNCE_START implicitly assigns the next
 * announce id (a per-stream ordinal starting at 0); the others reference it. Older versions send
 * a single ANNOUNCE_BROADCAST message that retracts by path (`ended`).
 */
export type AnnounceBroadcast =
	/** Before lite-07: a broadcast is now available, carrying the path suffix relative to the
	 * requested prefix, the hop chain, and (lite-06) the cost. An absent cost encodes as zero; it
	 * decodes as `undefined` on a wire with no room for one. */
	| { status: "active"; suffix: Path.Valid; hops?: Hop[]; cost?: Cost }
	/** Pre-lite-06: a broadcast is no longer available, retracted by path. */
	| { status: "ended"; suffix: Path.Valid }
	/** Lite06+: the claim with this announce id is gone, and the id retired; referencing it again
	 * is a protocol violation. On lite-07 its publisher withdrew it unless `lost`, when the
	 * sender only lost its way to it. */
	| { status: "endedId"; id: bigint; lost?: boolean }
	/** Lite06 only: atomically replace the hop chain and cost of the announcement with this id.
	 * The id stays live. */
	| { status: "update"; id: bigint; hops?: Hop[]; cost?: Cost }
	/** Lite07+ ANNOUNCE_START: a claim of the broadcast at `suffix`, relative to the session,
	 * serving `instance`, which assigns it the next announce id. */
	| { status: "start"; suffix: Path.Valid; pathBase?: Base; instance: Instance.Valid; claim: Claim }
	/** Lite07+ ANNOUNCE_UPDATE: the claim with this id now stands as `claim`. */
	| { status: "change"; id: bigint; claim: Claim }
	/** An unknown lite-06+ announce type, skipped by length. Does not assign an id. */
	| { status: "skipped" }
	/** Lite07+ SYNCED: the initial set of the prefix the receiver assigned this Prefix ID has been
	 * delivered. */
	| { status: "synced"; prefix: bigint }
	/** Lite07+ ANNOUNCE_REQUEST: the sender edits the prefixes it wants claims for, removing those
	 * with the Prefix IDs in `remove` and then adding each of `add`, which assigns it the next
	 * Prefix ID. With `more`, the edit continues in the next message, and takes effect whole with
	 * its last (see `requestParts`). */
	| { status: "request"; more: boolean; remove: bigint[]; add: Add[] }
	/** Lite07+ ANNOUNCE_OK: the receiver's oldest unanswered edit takes effect here. */
	| { status: "ok" };

/** A prefix an ANNOUNCE_REQUEST adds, relative to the session, and whether it also wants the
 * hidden claims beneath it. */
export interface Add {
	prefix: Path.Valid;
	hidden: boolean;
}

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

/** Encode what a lite-07 claim says after its epoch: cost, above, flags, priority, and chain. */
async function encodeClaim(w: Writer, claim: Claim) {
	if (claim.chain.length > CHAIN_MAX || claim.chain.some((node) => node === 0n))
		throw new Error("no node sends that chain");
	await w.u62(saturate(claim.cost));
	await w.u62(claim.above);
	await w.u8((claim.unavailable ? UNAVAILABLE : 0) | (claim.draining ? DRAINING : 0) | (claim.marked ? MARKED : 0));
	await w.u8(toWire(claim.priority));
	await w.u53(claim.chain.length);
	for (const node of claim.chain) await w.u62(node);
}

/** Decode a lite-07 claim of `epoch`. A chain past the limit or naming Node ID 0, and a flag this
 * version does not define, fail the stream: no sender builds one. Mirrors rs/moq-net. */
async function decodeClaim(r: Reader, epoch: bigint): Promise<Claim> {
	// Costs saturate at 2^62-1 on every version, so a larger one reads as the ceiling.
	const cost = saturate(await r.u62());
	const above = await r.u62();
	const flags = await r.u8();
	if (flags & ~(UNAVAILABLE | DRAINING | MARKED)) throw new ProtocolViolation(`unknown claim flags ${flags}`);
	const priority = fromWire(await r.u8());
	const count = await r.u62();
	if (count > BigInt(CHAIN_MAX)) throw new ProtocolViolation(`claim chain of ${count} nodes`);
	const chain: bigint[] = [];
	for (let i = 0n; i < count; i++) {
		const node = await r.u62();
		if (node === 0n) throw new ProtocolViolation("Node ID 0 names no node");
		chain.push(node);
	}
	return {
		epoch,
		cost,
		above,
		unavailable: (flags & UNAVAILABLE) !== 0,
		draining: (flags & DRAINING) !== 0,
		marked: (flags & MARKED) !== 0,
		priority,
		chain,
	};
}

/** Encode an ANNOUNCE_REQUEST body: `More`, the Prefix IDs to remove, then the prefixes to add,
 * each with its `Hidden`. */
async function encodeRequest(w: Writer, more: boolean, remove: bigint[], add: Add[]) {
	await w.bool(more);
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
	const more = await r.bool();
	const remove: bigint[] = [];
	for (let left = await r.u62(); left > 0n; left--) remove.push(await r.u62());
	const add: Add[] = [];
	for (let left = await r.u62(); left > 0n; left--) {
		add.push({ prefix: Path.decode(await r.string()), hidden: await r.bool() });
	}
	return { status: "request", more, remove, add };
}

/** The bytes one Add of `prefix` takes: its length, its bytes, and its flag. */
function addSize(prefix: Path.Valid, version: Version): number {
	const length = new TextEncoder().encode(Path.encode(prefix)).byteLength;
	return encodeVarint(length, version).byteLength + length + 1;
}

/** The bytes an ANNOUNCE_REQUEST body takes with `removes` Prefix IDs totalling `ids` bytes and
 * `adds` Adds totalling `added` bytes. */
function requestSize(removes: number, ids: number, adds: number, added: number, version: Version): number {
	return 1 + encodeVarint(removes, version).byteLength + encodeVarint(adds, version).byteLength + ids + added;
}

/** Whether the prefixes `set` fit the Adds of one ANNOUNCE_REQUEST, as a subscriber's whole set
 * must. */
export function requestFits(set: Iterable<Path.Valid>, version: Version): boolean {
	let adds = 0;
	let added = 0;
	for (const prefix of set) {
		adds++;
		added += addSize(prefix, version);
	}
	return requestSize(0, 0, adds, added, version) <= MAX_MESSAGE_SIZE;
}

/**
 * The ANNOUNCE_REQUESTs of one edit removing the Prefix IDs `remove` and adding `add`: as many as
 * it takes for each to fit one message, every one but the last with `more`. The receiver applies
 * them together at the last, so how they split changes nothing. Mirrors `request_parts` in
 * rs/moq-net.
 *
 * @internal
 */
export function requestParts(
	remove: bigint[],
	add: Add[],
	version: Version,
): { status: "request"; more: boolean; remove: bigint[]; add: Add[] }[] {
	const parts: { remove: bigint[]; add: Add[] }[] = [];
	let part: { remove: bigint[]; add: Add[] } = { remove: [], add: [] };
	let ids = 0;
	let added = 0;
	const fits = (id: number, adding: number) =>
		requestSize(
			part.remove.length + (id > 0 ? 1 : 0),
			ids + id,
			part.add.length + (adding > 0 ? 1 : 0),
			added + adding,
			version,
		) <= MAX_MESSAGE_SIZE;
	const next = () => {
		parts.push(part);
		part = { remove: [], add: [] };
		ids = 0;
		added = 0;
	};
	for (const id of remove) {
		const bytes = encodeVarint(id, version).byteLength;
		if (!fits(bytes, 0)) next();
		part.remove.push(id);
		ids += bytes;
	}
	for (const one of add) {
		const bytes = addSize(one.prefix, version);
		if (!fits(0, bytes)) next();
		part.add.push(one);
		added += bytes;
	}
	parts.push(part);
	return parts.map((part, i) => ({ status: "request", more: i + 1 < parts.length, ...part }));
}

// lite-06 message body (no discriminator; the type is carried outside the length prefix).
async function encodeAnnounce06Body(w: Writer, msg: AnnounceBroadcast, version: Version) {
	switch (msg.status) {
		case "active":
			if (hasRoutes(version)) throw new Error("a lite-07 claim names its instance");
			await encodePath(w, version, msg.suffix, undefined);
			await encodeRoute(w, version, undefined, undefined);
			await encodeHops(w, version, msg.hops);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "endedId":
			await w.u62(msg.id);
			if (hasRoutes(version)) await w.bool(msg.lost ?? false);
			break;
		case "update":
			if (hasRoutes(version)) throw new Error("a lite-07 update carries its claim");
			await w.u62(msg.id);
			await encodeHops(w, version, msg.hops);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "start":
			if (!hasRoutes(version)) throw new Error("claims not supported for this version");
			await encodePath(w, version, msg.suffix, msg.pathBase);
			await encodeRoute(w, version, msg.instance, msg.claim.epoch);
			await encodeClaim(w, msg.claim);
			break;
		case "change":
			if (!hasRoutes(version)) throw new Error("claims not supported for this version");
			await w.u62(msg.id);
			await w.u62(msg.claim.epoch);
			await encodeClaim(w, msg.claim);
			break;
		case "synced":
			await w.u62(msg.prefix);
			break;
		case "request":
			await encodeRequest(w, msg.more, msg.remove, msg.add);
			break;
		case "ok":
			break;
		case "ended":
			// The pre-lite-06 path-form retraction has no place on lite-06.
			throw new Error("ended-by-path not supported for this version");
		case "skipped":
			throw new Error("decode-only announce type cannot be encoded");
	}
}

// lite-06 outer message type for a given announcement.
function announce06Type(msg: AnnounceBroadcast, version: Version): number {
	switch (msg.status) {
		case "active":
		case "start":
			return ANNOUNCE_START;
		case "endedId":
			return ANNOUNCE_END;
		case "update":
		case "change":
			return ANNOUNCE_UPDATE;
		case "synced":
			if (!hasSynced(version)) throw new Error("synced not supported for this version");
			return SYNCED;
		case "request":
			if (!hasRoutes(version)) throw new Error("announce edits not supported for this version");
			return ANNOUNCE_REQUEST;
		case "ok":
			if (!hasRoutes(version)) throw new Error("announce edits not supported for this version");
			return ANNOUNCE_OK;
		case "ended":
			throw new Error("ended-by-path not supported for this version");
		case "skipped":
			throw new Error("decode-only announce type cannot be encoded");
	}
}

async function decodeAnnounce06Body(r: Reader, typ: number, version: Version): Promise<AnnounceBroadcast> {
	switch (typ) {
		case ANNOUNCE_START: {
			const path = await decodePath(r, version);
			const served = await decodeRoute(r, version);
			if (hasRoutes(version)) {
				// Every lite-07 claim names its instance.
				if (served.instance === undefined) throw new ProtocolViolation("a claim without an instance");
				const claim = await decodeClaim(r, served.epoch ?? 0n);
				return { status: "start", ...path, instance: served.instance, claim };
			}
			const hops = await decodeHops(r, version);
			return { status: "active", suffix: path.suffix, hops, cost: await decodeRouteCost(r, version) };
		}
		case ANNOUNCE_END: {
			const id = await r.u62();
			return hasRoutes(version) ? { status: "endedId", id, lost: await r.bool() } : { status: "endedId", id };
		}
		case ANNOUNCE_UPDATE: {
			const id = await r.u62();
			if (hasRoutes(version)) return { status: "change", id, claim: await decodeClaim(r, await r.u62()) };
			const hops = await decodeHops(r, version);
			return { status: "update", id, hops, cost: await decodeRouteCost(r, version) };
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
		case ANNOUNCE_FLOOR:
		case ROUTE:
		case ROUTE_END:
		case DEAD:
		case DRAIN:
			if (!hasRoutes(version)) break;
			// Only two nodes that both declared a Node ID exchange routes, and this one declares none.
			throw new ProtocolViolation(`route message ${typ} to a node that declared no Node ID`);
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
 * The live announcements on one lite-06+ announce stream, by Announce ID: what ANNOUNCE_END
 * and ANNOUNCE_UPDATE reference, and what a lite-07 base copies from. Every
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
		// TEMPORARY MIGRATION, delete at the next version cut and use Path.parts: the previous
		// release counts a keep by every `/`, escaped or not, and also speaks lite-07, so a keep
		// counted by segment would resolve to another path on a mixed-version link, as in
		// rs/moq-net's compress.rs.
		const head = base === Path.empty() ? [] : base.split("/");
		const keep = msg.pathBase.keep;
		if (keep > head.length) throw new ProtocolViolation(`announce keeps ${keep} of ${head.length} parts`);
		// Joined as text before parsing: a kept part may end in the `\` of an escaped `/`. A path
		// that does not resolve, such as one too deep, is refused as content, as rs/moq-net does.
		const kept = head.slice(0, keep);
		if (suffix !== Path.empty()) kept.push(suffix);
		return Path.tryDecode(kept.join("/"));
	}

	/** The suffix of the live announcement `id`, which an update references. */
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

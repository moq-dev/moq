import { ProtocolViolation } from "../error.ts";
import { type Cost, type Hop, HopSchema, MAX_HOPS, UNKNOWN_HOP } from "../hop.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import {
	hasAnnounceCompression,
	hasAnnounceId,
	hasAnnounceOk,
	hasExcludeHop,
	hasHidden,
	hasRouteCost,
	Version,
} from "./version.ts";

// Pre-lite-06 inner status values, carried inside the single ANNOUNCE_BROADCAST body.
const STATUS_ENDED = 0;
const STATUS_ACTIVE = 1;
const STATUS_RESTART = 2;

// lite-06 announce message types: an outer discriminator carried before the length
// prefix, so each announcement is an independently-typed, length-delimited message
// (mirroring SUBSCRIBE_START/END/DROP on the subscribe stream).
const ANNOUNCE_START = 0;
const ANNOUNCE_END = 1;
const ANNOUNCE_RESTART = 2;

export type { Cost };

/**
 * Lite-07: copy `keep` path segments (from the head) or hops (from the tail) of the
 * live announcement `distance` back from the stream's next Announce ID, where 1 is the
 * latest ANNOUNCE_START. Resolved by {@link AnnounceHistory}.
 */
export type Base = { distance: bigint; keep: number };

/**
 * An announcement on the Announce Stream, advertising or retracting a broadcast.
 *
 * On lite-06+ these are three independently-typed messages (`ANNOUNCE_START`,
 * `ANNOUNCE_END`, `ANNOUNCE_RESTART`), each framed as `Type | Length | Body` like the
 * subscribe stream's responses. Each `active` (ANNOUNCE_START) implicitly assigns the
 * next announce id (a per-stream ordinal starting at 0); `endedId`/`restart` reference
 * that id instead of repeating the path. Older versions send a single ANNOUNCE_BROADCAST
 * message that retracts by path (`ended`).
 */
export type AnnounceBroadcast =
	/** A broadcast is now available, carrying the path suffix, the hop chain, and
	 * (lite-06+) the route cost. An absent cost encodes as zero; it decodes as
	 * `undefined` on a wire with no room for one. On lite-07, `suffix` follows the
	 * segments `pathBase` copies and `hops` precede the ones `hopBase` copies. */
	| { status: "active"; suffix: Path.Valid; hops: Hop[]; cost?: Cost; pathBase?: Base; hopBase?: Base }
	/** Pre-lite-06: a broadcast is no longer available, retracted by path. */
	| { status: "ended"; suffix: Path.Valid }
	/** Lite06+: a broadcast is no longer available, retracted by announce id.
	 * The id is retired; referencing it again is a protocol violation. */
	| { status: "endedId"; id: bigint }
	/** Lite06+: atomically replace the announcement with this id (e.g. a new hop
	 * chain after a relay failover, or a route whose cost moved). The id stays live. */
	| { status: "restart"; id: bigint; hops: Hop[]; cost?: Cost; hopBase?: Base }
	/** An unknown lite-06+ announce type, skipped by length. Does not assign an id. */
	| { status: "skipped" };

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

async function encodeHops(w: Writer, version: Version, hops: Hop[]) {
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
				await w.u62(origin);
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

// Lite-07 carries a base around a path and a hop list. A distance of 0 names
// nothing, and a keep without a base is a violation. Other versions have no room for
// a base at all.
function checkBase(version: Version, base: Base | undefined) {
	if (base && !hasAnnounceCompression(version)) {
		throw new Error("announce compression not supported for this version");
	}
}

function toBase(distance: bigint, keep: number): Base | undefined {
	if (distance !== 0n) return { distance, keep };
	if (keep !== 0) throw new ProtocolViolation("announce keep without a base");
	return undefined;
}

async function encodePath(w: Writer, version: Version, suffix: Path.Valid, base: Base | undefined) {
	checkBase(version, base);
	if (hasAnnounceCompression(version)) {
		await w.u62(base?.distance ?? 0n);
		await w.u53(base?.keep ?? 0);
	}
	await w.string(Path.encode(suffix));
}

async function decodePath(r: Reader, version: Version): Promise<{ suffix: Path.Valid; pathBase?: Base }> {
	if (!hasAnnounceCompression(version)) return { suffix: Path.decode(await r.string()) };
	const pathBase = toBase(await r.u62(), await r.u53());
	const suffix = Path.decode(await r.string());
	return pathBase ? { suffix, pathBase } : { suffix };
}

async function encodeHopsBlock(w: Writer, version: Version, hops: Hop[], base: Base | undefined) {
	checkBase(version, base);
	if (!hasAnnounceCompression(version)) return encodeHops(w, version, hops);
	await w.u62(base?.distance ?? 0n);
	await encodeHops(w, version, hops);
	await w.u53(base?.keep ?? 0);
}

async function decodeHopsBlock(r: Reader, version: Version): Promise<{ hops: Hop[]; hopBase?: Base }> {
	if (!hasAnnounceCompression(version)) return { hops: await decodeHops(r, version) };
	const distance = await r.u62();
	const hops = await decodeHops(r, version);
	const hopBase = toBase(distance, await r.u53());
	return hopBase ? { hops, hopBase } : { hops };
}

// The route cost rides lite-06+ announcements as two varints, warm then cold; older
// versions carry neither.
async function encodeRouteCost(w: Writer, version: Version, cost: Cost | undefined) {
	if (!hasRouteCost(version)) return;
	await w.u62(cost?.warm ?? 0n);
	await w.u62(cost?.cold ?? 0n);
}

async function decodeRouteCost(r: Reader, version: Version): Promise<Cost | undefined> {
	if (!hasRouteCost(version)) return undefined;
	return { warm: await r.u62(), cold: await r.u62() };
}

// lite-06 message body (no discriminator; the type is carried outside the length prefix).
async function encodeAnnounce06Body(w: Writer, msg: AnnounceBroadcast, version: Version) {
	switch (msg.status) {
		case "active":
			await encodePath(w, version, msg.suffix, msg.pathBase);
			await encodeHopsBlock(w, version, msg.hops, msg.hopBase);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "endedId":
			await w.u62(msg.id);
			break;
		case "restart":
			await w.u62(msg.id);
			await encodeHopsBlock(w, version, msg.hops, msg.hopBase);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "ended":
			// The pre-lite-06 path-form retraction has no place on lite-06.
			throw new Error("ended-by-path not supported for this version");
		case "skipped":
			throw new Error("decode-only announce type cannot be encoded");
	}
}

// lite-06 outer message type for a given announcement.
function announce06Type(msg: AnnounceBroadcast): number {
	switch (msg.status) {
		case "active":
			return ANNOUNCE_START;
		case "endedId":
			return ANNOUNCE_END;
		case "restart":
			return ANNOUNCE_RESTART;
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
			const hops = await decodeHopsBlock(r, version);
			return { status: "active", ...path, ...hops, cost: await decodeRouteCost(r, version) };
		}
		case ANNOUNCE_END:
			return { status: "endedId", id: await r.u62() };
		case ANNOUNCE_RESTART: {
			const id = await r.u62();
			const hops = await decodeHopsBlock(r, version);
			return { status: "restart", id, ...hops, cost: await decodeRouteCost(r, version) };
		}
		default:
			// Skip the length-prefixed body so an earlier Lite06 build negotiating
			// the same ALPN does not kill the announce stream.
			await r.readAll();
			return { status: "skipped" };
	}
}

// Pre-lite-06 single ANNOUNCE_BROADCAST body: an inner status byte, then path + hops.
async function encodeLegacyBody(w: Writer, msg: AnnounceBroadcast, version: Version) {
	switch (msg.status) {
		case "active":
			checkBase(version, msg.pathBase ?? msg.hopBase);
			await w.u8(STATUS_ACTIVE);
			await w.string(Path.encode(msg.suffix));
			await encodeHops(w, version, msg.hops);
			break;
		case "ended":
			await w.u8(STATUS_ENDED);
			await w.string(Path.encode(msg.suffix));
			await encodeHops(w, version, []);
			break;
		case "endedId":
		case "restart":
		case "skipped":
			// The id-referencing forms only exist on lite-06+.
			throw new Error("announce ids not supported for this version");
	}
}

async function decodeLegacyBody(r: Reader, version: Version): Promise<AnnounceBroadcast> {
	const status = await r.u8();
	// On lite-05 a restart travels as a duplicate `active`, but the explicit restart
	// status is accepted on decode and treated the same. Older versions never defined it.
	const active = status === STATUS_ACTIVE || (status === STATUS_RESTART && hasAnnounceOk(version));
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
		await w.u53(announce06Type(msg));
		return Message.encode(w, (w) => encodeAnnounce06Body(w, msg, version));
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

type Advertised = { suffix: Path.Valid; hops: Hop[] };

/**
 * The live announcements on one lite-06+ announce stream, by Announce ID: what
 * ANNOUNCE_END and ANNOUNCE_UPDATE reference, and what a lite-07 base copies from.
 * Every violation throws {@link ProtocolViolation}.
 */
export class AnnounceHistory {
	#next = 0n;
	#live = new Map<bigint, Advertised>();

	#base(base: Base): Advertised {
		const live = this.#live.get(this.#next - base.distance);
		if (!live) throw new ProtocolViolation(`announce base ${base.distance} is not live`);
		return live;
	}

	#hops(hops: Hop[], base: Base | undefined): Hop[] {
		if (!base) return hops;
		const tail = this.#base(base).hops;
		if (base.keep > tail.length) throw new ProtocolViolation(`announce keeps ${base.keep} of ${tail.length} hops`);
		const resolved = [...hops, ...tail.slice(tail.length - base.keep)];
		checkHops(resolved);
		return resolved;
	}

	/** An ANNOUNCE_START: resolve it and assign it the next id. */
	start(msg: { suffix: Path.Valid; hops: Hop[]; pathBase?: Base; hopBase?: Base }): Advertised {
		let suffix = msg.suffix;
		if (msg.pathBase) {
			const head = Path.parts(this.#base(msg.pathBase).suffix);
			const keep = msg.pathBase.keep;
			if (keep > head.length) throw new ProtocolViolation(`announce keeps ${keep} of ${head.length} segments`);
			suffix = Path.join(Path.from(...head.slice(0, keep)), suffix);
			if (Path.parts(suffix).length > Path.MAX_PARTS) {
				throw new ProtocolViolation(`path exceeds ${Path.MAX_PARTS} parts`);
			}
		}
		const hops = this.#hops(msg.hops, msg.hopBase);
		this.#live.set(this.#next++, { suffix, hops });
		return { suffix, hops };
	}

	/** An ANNOUNCE_UPDATE: resolve its chain before replacing the old one, which it may be based on. */
	update(msg: { id: bigint; hops: Hop[]; hopBase?: Base }): Advertised {
		const hops = this.#hops(msg.hops, msg.hopBase);
		const live = this.#live.get(msg.id);
		if (!live) throw new ProtocolViolation(`unknown announce id: ${msg.id}`);
		this.#live.set(msg.id, { suffix: live.suffix, hops });
		return { suffix: live.suffix, hops };
	}

	/** An ANNOUNCE_END: retire the id, returning its suffix. */
	end(id: bigint): Path.Valid {
		const live = this.#live.get(id);
		if (!live) throw new ProtocolViolation(`unknown announce id: ${id}`);
		this.#live.delete(id);
		return live.suffix;
	}
}

/**
 * ANNOUNCE_REQUEST: sent by the subscriber to request ANNOUNCE_BROADCAST messages
 * for a path prefix. Renamed from `AnnounceInterest` in lite-05.
 */
export class AnnounceRequest {
	prefix: Path.Valid;
	/** Lite04/05 only: the 62-bit Hop id of the peer asking for announces, which the
	 * publisher uses to skip announces that already passed through it. Zero means "no
	 * exclusion". Not on the wire elsewhere, so a value set here is ignored when encoding
	 * for another version and decodes as zero.
	 *
	 * Must be a bigint: peer origins are up to 62 bits and overflow u53. */
	excludeHop: bigint;
	/** Lite07+: also announce routes with a `.`-prefixed segment below the prefix. Not on
	 * the wire earlier, so a value set here is ignored when encoding for an older version
	 * and decodes as false. */
	hidden: boolean;

	constructor(prefix: Path.Valid, excludeHop: bigint = 0n, hidden = false) {
		this.prefix = prefix;
		this.excludeHop = excludeHop;
		this.hidden = hidden;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.prefix));
		if (hasExcludeHop(version)) {
			await w.u62(this.excludeHop);
		}
		if (hasHidden(version)) {
			await w.bool(this.hidden);
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceRequest> {
		const prefix = Path.decode(await r.string());
		const excludeHop = hasExcludeHop(version) ? await r.u62() : 0n;
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

	static async #decode(r: Reader): Promise<AnnounceInit> {
		const count = await r.u53();
		const suffixes: Path.Valid[] = [];
		for (let i = 0; i < count; i++) {
			suffixes.push(Path.decode(await r.string()));
		}
		return new AnnounceInit(suffixes);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		AnnounceInit.#guard(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceInit> {
		AnnounceInit.#guard(version);
		return Message.decode(r, AnnounceInit.#decode);
	}
}

/// Sent by the publisher as the first message on an announce stream, before any
/// individual Announce messages. Lite05+ only; the successor to AnnounceInit.
///
/// `origin` is the responder's Hop ID, which the subscriber stamps onto each
/// announce's hop chain (the publisher no longer stamps itself), or the reserved
/// {@link UNKNOWN_HOP} when the responder has no identity to give. `active` is
/// the number of initial Announce messages that follow immediately.
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

	async #encode(w: Writer) {
		await w.u62(this.hop);
		await w.u53(this.active);
	}

	static async #decode(r: Reader): Promise<AnnounceOk> {
		// The draft reserves 0 for "unknown": the responder was never assigned an id, or
		// withholds it to obscure its routing. It names nobody, so callers must not stamp
		// it onto a hop chain, but it is a legal message and not grounds to drop the stream.
		const origin = HopSchema.parse(await r.u62());
		const active = await r.u53();
		return new AnnounceOk(origin, active);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		AnnounceOk.#guard(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceOk> {
		AnnounceOk.#guard(version);
		return Message.decode(r, AnnounceOk.#decode);
	}
}

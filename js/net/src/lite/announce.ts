import { ProtocolViolation } from "../error.ts";
import { type Cost, type Hop, HopSchema, MAX_HOPS, UNKNOWN_HOP } from "../hop.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { hasAnnounceId, hasAnnounceOk, hasExcludeHop, hasRouteCost, Version } from "./version.ts";

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
const ANNOUNCE_PATTERN = 3;

const SEGMENT_LITERAL = 0;
const SEGMENT_WILDCARD = 1;
const SEGMENT_GLOBSTAR = 2;
const SEGMENT_PARTIAL = 3;

export type { Cost };

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
	 * `undefined` on a wire with no room for one. */
	| { status: "active"; suffix: Path.Valid; hops: Hop[]; cost?: Cost }
	/** Pre-lite-06: a broadcast is no longer available, retracted by path. */
	| { status: "ended"; suffix: Path.Valid }
	/** Lite06+: a broadcast is no longer available, retracted by announce id.
	 * The id is retired; referencing it again is a protocol violation. */
	| { status: "endedId"; id: bigint }
	/** Lite06+: atomically replace the announcement with this id (e.g. a new hop
	 * chain after a relay failover, or a route whose cost moved). The id stays live. */
	| { status: "restart"; id: bigint; hops: Hop[]; cost?: Cost }
	/** Lite06+: a route over a path pattern, relative to the requested prefix.
	 * Assigns the next announce id like `active`. The cost is a single value. */
	| { status: "pattern"; pattern: Path.Pattern; hops: Hop[]; cost: bigint }
	/** Lite06+: ANNOUNCE_PATTERN whose segments included an unknown kind. Track
	 * the id; do not select or forward. */
	| { status: "ignored"; hops: Hop[]; cost: bigint }
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
			await w.string(Path.encode(msg.suffix));
			await encodeHops(w, version, msg.hops);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "endedId":
			await w.u62(msg.id);
			break;
		case "restart":
			await w.u62(msg.id);
			await encodeHops(w, version, msg.hops);
			await encodeRouteCost(w, version, msg.cost);
			break;
		case "pattern":
			await encodePattern(w, msg.pattern);
			await encodeHops(w, version, msg.hops);
			await w.u62(msg.cost);
			break;
		case "ended":
			// The pre-lite-06 path-form retraction has no place on lite-06.
			throw new Error("ended-by-path not supported for this version");
		case "ignored":
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
		case "pattern":
			return ANNOUNCE_PATTERN;
		case "ended":
			throw new Error("ended-by-path not supported for this version");
		case "ignored":
		case "skipped":
			throw new Error("decode-only announce type cannot be encoded");
	}
}

async function decodeAnnounce06Body(r: Reader, typ: number, version: Version): Promise<AnnounceBroadcast> {
	switch (typ) {
		case ANNOUNCE_START: {
			const suffix = Path.decode(await r.string());
			const hops = await decodeHops(r, version);
			return { status: "active", suffix, hops, cost: await decodeRouteCost(r, version) };
		}
		case ANNOUNCE_END:
			return { status: "endedId", id: await r.u62() };
		case ANNOUNCE_RESTART: {
			const id = await r.u62();
			const hops = await decodeHops(r, version);
			return { status: "restart", id, hops, cost: await decodeRouteCost(r, version) };
		}
		case ANNOUNCE_PATTERN: {
			const pattern = await decodePattern(r);
			const hops = await decodeHops(r, version);
			const cost = await r.u62();
			if (!pattern) return { status: "ignored", hops, cost };
			return { status: "pattern", pattern, hops, cost };
		}
		default:
			// Skip the length-prefixed body so an earlier Lite06 build negotiating
			// the same ALPN does not kill the announce stream.
			await r.readAll();
			return { status: "skipped" };
	}
}

async function encodePattern(w: Writer, pattern: Path.Pattern) {
	await w.u53(pattern.segments.length);
	for (const segment of pattern.segments) {
		await encodeSegment(w, segment);
	}
}

async function encodeSegment(w: Writer, segment: Path.Segment) {
	let kind: number;
	let value: Uint8Array;
	switch (segment.kind) {
		case "literal":
			kind = SEGMENT_LITERAL;
			value = new TextEncoder().encode(segment.value);
			break;
		case "wildcard":
			kind = SEGMENT_WILDCARD;
			value = new Uint8Array();
			break;
		case "globstar":
			kind = SEGMENT_GLOBSTAR;
			value = new Uint8Array();
			break;
		case "partial": {
			kind = SEGMENT_PARTIAL;
			const prefix = new TextEncoder().encode(segment.prefix);
			const suffix = new TextEncoder().encode(segment.suffix);
			const scratch: Uint8Array[] = [];
			const writer = new Writer(
				new WritableStream<Uint8Array>({ write: (chunk) => void scratch.push(new Uint8Array(chunk)) }),
			);
			await writer.u53(prefix.byteLength);
			if (prefix.byteLength > 0) await writer.write(prefix);
			if (suffix.byteLength > 0) await writer.write(suffix);
			writer.close();
			await writer.closed;
			const total = scratch.reduce((sum, c) => sum + c.byteLength, 0);
			value = new Uint8Array(total);
			let offset = 0;
			for (const chunk of scratch) {
				value.set(chunk, offset);
				offset += chunk.byteLength;
			}
			break;
		}
	}
	await w.u53(kind);
	await w.u53(value.byteLength);
	if (value.byteLength > 0) await w.write(value);
}

async function decodePattern(r: Reader): Promise<Path.Pattern | undefined> {
	const count = await r.u53();
	if (count > Path.MAX_PARTS) throw new Error("pattern exceeds path limit");
	const segments: Path.Segment[] = [];
	let ignored = false;
	for (let i = 0; i < count; i++) {
		const segment = await decodeSegment(r);
		if (!segment) ignored = true;
		else if (!ignored) segments.push(segment);
	}
	if (ignored) return undefined;
	return Path.Pattern.from(segments);
}

async function decodeSegment(r: Reader): Promise<Path.Segment | undefined> {
	const kind = await r.u53();
	const size = await r.u53();
	const value = size > 0 ? await r.read(size) : new Uint8Array();
	switch (kind) {
		case SEGMENT_LITERAL: {
			const literal = new TextDecoder().decode(value);
			if (!literal || literal.includes("/") || literal.includes("*")) {
				throw new Error("invalid literal segment");
			}
			return { kind: "literal", value: literal };
		}
		case SEGMENT_WILDCARD:
			if (value.byteLength > 0) throw new Error("wildcard value must be empty");
			return { kind: "wildcard" };
		case SEGMENT_GLOBSTAR:
			if (value.byteLength > 0) throw new Error("globstar value must be empty");
			return { kind: "globstar" };
		case SEGMENT_PARTIAL: {
			const inner = new Reader(undefined, value);
			const prefixLen = await inner.u53();
			const prefixBytes = prefixLen > 0 ? await inner.read(prefixLen) : new Uint8Array();
			const suffixBytes = await inner.readAll();
			const prefix = new TextDecoder().decode(prefixBytes);
			const suffix = new TextDecoder().decode(suffixBytes);
			if ((!prefix && !suffix) || /[*/]/.test(prefix) || /[*/]/.test(suffix)) {
				throw new Error("invalid partial segment");
			}
			return { kind: "partial", prefix, suffix };
		}
		default:
			return undefined;
	}
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
		case "endedId":
		case "restart":
		case "pattern":
		case "ignored":
		case "skipped":
			// The id-referencing and pattern forms only exist on lite-06+.
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

	constructor(prefix: Path.Valid, excludeHop: bigint = 0n) {
		this.prefix = prefix;
		this.excludeHop = excludeHop;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.prefix));
		if (hasExcludeHop(version)) {
			await w.u62(this.excludeHop);
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceRequest> {
		const prefix = Path.decode(await r.string());
		const excludeHop = hasExcludeHop(version) ? await r.u62() : 0n;
		return new AnnounceRequest(prefix, excludeHop);
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

import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { type Origin, OriginSchema } from "./origin.ts";
import { hasAnnounceId, hasAnnounceOk, Version } from "./version.ts";

// Must match the MAX_HOPS in Rust's model/origin.rs. Broadcasts with longer
// hop chains are rejected; this keeps loop-detection bounded and rejects
// pathological announcements across clusters with unbounded forwarding.
export const MAX_HOPS = 32;

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
	/** A broadcast is now available, carrying the path suffix and the hop chain. */
	| { status: "active"; suffix: Path.Valid; hops: Origin[] }
	/** Pre-lite-06: a broadcast is no longer available, retracted by path. */
	| { status: "ended"; suffix: Path.Valid }
	/** Lite06+: a broadcast is no longer available, retracted by announce id.
	 * The id is retired; referencing it again is a protocol violation. */
	| { status: "endedId"; id: bigint }
	/** Lite06+: atomically replace the announcement with this id (e.g. a new hop
	 * chain after a relay failover). The id stays live. */
	| { status: "restart"; id: bigint; hops: Origin[] };

function checkHops(hops: Origin[]) {
	if (hops.length > MAX_HOPS) {
		throw new Error(`hop count ${hops.length} exceeds maximum ${MAX_HOPS}`);
	}
}

async function encodeHops(w: Writer, version: Version, hops: Origin[]) {
	checkHops(hops);
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			break;
		case Version.DRAFT_03:
			await w.u53(hops.length);
			break;
		default:
			// Lite04+: hop count + individual Origin varints.
			await w.u53(hops.length);
			for (const origin of hops) {
				await w.u62(origin);
			}
			break;
	}
}

async function decodeHops(r: Reader, version: Version): Promise<Origin[]> {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			return [];
		case Version.DRAFT_03: {
			const count = await r.u53();
			if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
			// Lite03 carries only a hop count, not individual ids. Fill with
			// the zero placeholder (OriginSchema accepts 0 as valid on-wire).
			const placeholder = OriginSchema.parse(0n);
			return new Array<Origin>(count).fill(placeholder);
		}
		default: {
			// Lite04+: hop count + individual Origin varints.
			const count = await r.u53();
			if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
			const hops: Origin[] = [];
			for (let i = 0; i < count; i++) {
				hops.push(OriginSchema.parse(await r.u62()));
			}
			return hops;
		}
	}
}

// lite-06 message body (no discriminator; the type is carried outside the length prefix).
async function encodeAnnounce06Body(w: Writer, msg: AnnounceBroadcast, version: Version) {
	switch (msg.status) {
		case "active":
			await w.string(Path.encode(msg.suffix));
			await encodeHops(w, version, msg.hops);
			break;
		case "endedId":
			await w.u62(msg.id);
			break;
		case "restart":
			await w.u62(msg.id);
			await encodeHops(w, version, msg.hops);
			break;
		case "ended":
			// The pre-lite-06 path-form retraction has no place on lite-06.
			throw new Error("ended-by-path not supported for this version");
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
	}
}

async function decodeAnnounce06Body(r: Reader, typ: number, version: Version): Promise<AnnounceBroadcast> {
	switch (typ) {
		case ANNOUNCE_START:
			return { status: "active", suffix: Path.decode(await r.string()), hops: await decodeHops(r, version) };
		case ANNOUNCE_END:
			return { status: "endedId", id: await r.u62() };
		case ANNOUNCE_RESTART:
			return { status: "restart", id: await r.u62(), hops: await decodeHops(r, version) };
		default:
			throw new Error(`unknown announce message type: ${typ}`);
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

/**
 * ANNOUNCE_REQUEST: sent by the subscriber to request ANNOUNCE_BROADCAST messages
 * for a path prefix. Renamed from `AnnounceInterest` in lite-05.
 */
export class AnnounceRequest {
	prefix: Path.Valid;
	// 62-bit Origin id of the peer asking for announces. Zero means "no exclusion".
	// Must be a bigint: peer origins are up to 62 bits and overflow u53.
	excludeHop: bigint;

	constructor(prefix: Path.Valid, excludeHop: bigint = 0n) {
		this.prefix = prefix;
		this.excludeHop = excludeHop;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.prefix));
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
			case Version.DRAFT_03:
				break;
			default:
				// Lite04+: exclude_hop field (u62 varint).
				await w.u62(this.excludeHop);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceRequest> {
		const prefix = Path.decode(await r.string());
		let excludeHop = 0n;
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
			case Version.DRAFT_03:
				break;
			default:
				excludeHop = await r.u62();
				break;
		}
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
/// `origin` is the responder's origin id, which the subscriber stamps onto each
/// announce's hop chain (the publisher no longer stamps itself). `active` is the
/// number of initial Announce messages that follow immediately.
export class AnnounceOk {
	origin: Origin;
	active: number;

	constructor(origin: Origin, active: number) {
		this.origin = origin;
		this.active = active;
	}

	static #guard(version: Version) {
		if (!hasAnnounceOk(version)) {
			throw new Error("announce ok not supported for this version");
		}
	}

	async #encode(w: Writer) {
		await w.u62(this.origin);
		await w.u53(this.active);
	}

	static async #decode(r: Reader): Promise<AnnounceOk> {
		const raw = await r.u62();
		// A zero responder id is never legitimate; it would stamp a placeholder onto chains.
		if (raw === 0n) throw new Error("announce ok origin must be non-zero");
		const origin = OriginSchema.parse(raw);
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

import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { type Origin, OriginSchema } from "./origin.ts";
import { hasAnnounceOk, Version } from "./version.ts";

// Must match the MAX_HOPS in Rust's model/origin.rs. Broadcasts with longer
// hop chains are rejected; this keeps loop-detection bounded and rejects
// pathological announcements across clusters with unbounded forwarding.
export const MAX_HOPS = 32;

const ANNOUNCE_ENDED = 0;
const ANNOUNCE_ACTIVE = 1;
const ANNOUNCE_RESTART = 2;

/**
 * ANNOUNCE_BROADCAST: sent by the publisher to advertise (or retract) a broadcast.
 *
 * Carries the broadcast path suffix and the hop chain. Renamed from `Announce` in lite-05.
 */
export class AnnounceBroadcast {
	suffix: Path.Valid;
	active: boolean;
	hops: Origin[];

	constructor(props: { suffix: Path.Valid; active: boolean; hops?: Origin[] }) {
		this.suffix = props.suffix;
		this.active = props.active;
		this.hops = props.hops ?? [];
		if (this.hops.length > MAX_HOPS) {
			throw new Error(`hop count ${this.hops.length} exceeds maximum ${MAX_HOPS}`);
		}
	}

	async #encode(w: Writer, version: Version) {
		await w.u8(this.active ? ANNOUNCE_ACTIVE : ANNOUNCE_ENDED);
		await w.string(Path.encode(this.suffix));

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			case Version.DRAFT_03:
				await w.u53(this.hops.length);
				break;
			default:
				// Lite04+: hop count + individual Origin varints.
				await w.u53(this.hops.length);
				for (const origin of this.hops) {
					await w.u62(origin);
				}
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceBroadcast> {
		const status = await r.u8();
		const active = status === ANNOUNCE_ACTIVE || (status === ANNOUNCE_RESTART && hasAnnounceOk(version));
		if (status !== ANNOUNCE_ENDED && status !== ANNOUNCE_ACTIVE && !active) {
			throw new Error("invalid announce status");
		}
		const suffix = Path.decode(await r.string());

		let hops: Origin[] = [];
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			case Version.DRAFT_03: {
				const count = await r.u53();
				if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
				// Lite03 carries only a hop count, not individual ids. Fill with
				// the zero placeholder (OriginSchema accepts 0 as valid on-wire).
				const placeholder = OriginSchema.parse(0n);
				hops = new Array<Origin>(count).fill(placeholder);
				break;
			}
			default: {
				// Lite04+: hop count + individual Origin varints.
				const count = await r.u53();
				if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
				hops = [];
				for (let i = 0; i < count; i++) {
					hops.push(OriginSchema.parse(await r.u62()));
				}
				break;
			}
		}

		return new AnnounceBroadcast({ suffix, active, hops });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceBroadcast> {
		return Message.decode(r, (r) => AnnounceBroadcast.#decode(r, version));
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<AnnounceBroadcast | undefined> {
		return Message.decodeMaybe(r, (r) => AnnounceBroadcast.#decode(r, version));
	}
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

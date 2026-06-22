import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { type Origin, OriginSchema } from "./origin.ts";
import { hasBroadcastEpoch, hopsFixedWidth, Version } from "./version.ts";

// Must match the MAX_HOPS in Rust's model/origin.rs. Broadcasts with longer
// hop chains are rejected; this keeps loop-detection bounded and rejects
// pathological announcements across clusters with unbounded forwarding.
export const MAX_HOPS = 32;

/**
 * Seconds between the Unix epoch and 2020-01-01T00:00:00 UTC.
 *
 * Broadcast epochs ride the wire as milliseconds since this base (smaller than a
 * Unix-epoch value, and good past the year 2500 in a varint). See {@link epochNow}.
 */
export const EPOCH_BASE_SECONDS = 1_577_836_800;

/**
 * The current wall clock as a broadcast epoch: whole milliseconds since
 * 2020-01-01 UTC (the wire value). Saturates to `0` for a clock before the base.
 */
export function epochNow(): number {
	return Math.max(0, Math.floor(Date.now() - EPOCH_BASE_SECONDS * 1000));
}

/**
 * ANNOUNCE_BROADCAST: sent by the publisher to advertise (or retract) a broadcast.
 *
 * Carries the broadcast path suffix, its instance {@link epoch} (lite-05+), and the
 * hop chain. Renamed from `Announce` in lite-05.
 */
export class AnnounceBroadcast {
	suffix: Path.Valid;
	active: boolean;
	/**
	 * Broadcast instance epoch: milliseconds since 2020-01-01 UTC (see {@link epochNow}).
	 * Only carried on the wire for lite-05+; `0` on older versions.
	 */
	epoch: number;
	hops: Origin[];

	constructor(props: { suffix: Path.Valid; active: boolean; epoch?: number; hops?: Origin[] }) {
		this.suffix = props.suffix;
		this.active = props.active;
		this.epoch = props.epoch ?? 0;
		this.hops = props.hops ?? [];
		if (this.hops.length > MAX_HOPS) {
			throw new Error(`hop count ${this.hops.length} exceeds maximum ${MAX_HOPS}`);
		}
	}

	async #encode(w: Writer, version: Version) {
		await w.bool(this.active);
		await w.string(this.suffix);

		// Lite05+: the epoch varint sits after the suffix and before the hop chain.
		if (hasBroadcastEpoch(version)) {
			await w.u53(this.epoch);
		}

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			case Version.DRAFT_03:
				await w.u53(this.hops.length);
				break;
			default:
				// Lite04+: hop count + individual Hop IDs. Lite05+ carries each id
				// fixed-width (64-bit); Lite04 used a 62-bit varint.
				await w.u53(this.hops.length);
				for (const origin of this.hops) {
					if (hopsFixedWidth(version)) {
						await w.u64(origin);
					} else {
						await w.u62(origin);
					}
				}
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceBroadcast> {
		const active = await r.bool();
		const suffix = Path.from(await r.string());

		// Lite05+ carries the epoch after the suffix; older versions default it to 0.
		const epoch = hasBroadcastEpoch(version) ? await r.u53() : 0;

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
				// Lite04+: hop count + individual Hop IDs. Lite05+ carries each id
				// fixed-width (64-bit); Lite04 used a 62-bit varint.
				const count = await r.u53();
				if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
				hops = [];
				for (let i = 0; i < count; i++) {
					hops.push(OriginSchema.parse(hopsFixedWidth(version) ? await r.u64() : await r.u62()));
				}
				break;
			}
		}

		return new AnnounceBroadcast({ suffix, active, epoch, hops });
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
	// Hop ID of the peer asking for announces. Zero means "no exclusion".
	// Must be a bigint: peer origins are up to 64 bits and overflow u53.
	excludeHop: bigint;

	constructor(prefix: Path.Valid, excludeHop: bigint = 0n) {
		this.prefix = prefix;
		this.excludeHop = excludeHop;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(this.prefix);
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
			case Version.DRAFT_03:
				break;
			default:
				// Lite04+: exclude_hop Hop ID. Lite05+ fixed-width (64-bit); Lite04 a 62-bit varint.
				if (hopsFixedWidth(version)) {
					await w.u64(this.excludeHop);
				} else {
					await w.u62(this.excludeHop);
				}
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceRequest> {
		const prefix = Path.from(await r.string());
		let excludeHop = 0n;
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
			case Version.DRAFT_03:
				break;
			default:
				excludeHop = hopsFixedWidth(version) ? await r.u64() : await r.u62();
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
			await w.string(path);
		}
	}

	static async #decode(r: Reader): Promise<AnnounceInit> {
		const count = await r.u53();
		const suffixes: Path.Valid[] = [];
		for (let i = 0; i < count; i++) {
			suffixes.push(Path.from(await r.string()));
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
		switch (version) {
			case Version.DRAFT_05_WIP:
				break;
			default:
				throw new Error("announce ok not supported for this version");
		}
	}

	async #encode(w: Writer) {
		// lite-05 carries the Hop ID fixed-width (64-bit).
		await w.u64(this.origin);
		await w.u53(this.active);
	}

	static async #decode(r: Reader): Promise<AnnounceOk> {
		const raw = await r.u64();
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

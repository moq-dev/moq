import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

export class SubscribeUpdate {
	priority: number;
	ordered: boolean;
	maxLatency: number;
	startGroup?: number;
	endGroup?: number;

	constructor(props: {
		priority: number;
		ordered?: boolean;
		maxLatency?: number;
		startGroup?: number;
		endGroup?: number;
	}) {
		this.priority = props.priority;
		this.ordered = props.ordered ?? false;
		this.maxLatency = props.maxLatency ?? 0;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
	}

	async #encode(w: Writer, version: Version) {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				await w.u8(this.priority);
				break;
			default:
				await w.u8(this.priority);
				await w.bool(this.ordered);
				await w.u53(this.maxLatency);
				await w.u53(this.startGroup !== undefined ? this.startGroup + 1 : 0);
				await w.u53(this.endGroup !== undefined ? this.endGroup + 1 : 0);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<SubscribeUpdate> {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new SubscribeUpdate({ priority: await r.u8() });
			default: {
				const priority = await r.u8();
				const ordered = await r.bool();
				const maxLatency = await r.u53();
				const startGroup = await r.u53();
				const endGroup = await r.u53();
				return new SubscribeUpdate({
					priority,
					ordered,
					maxLatency,
					startGroup: startGroup > 0 ? startGroup - 1 : undefined,
					endGroup: endGroup > 0 ? endGroup - 1 : undefined,
				});
			}
		}
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeUpdate> {
		return Message.decode(r, (r) => SubscribeUpdate.#decode(r, version));
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<SubscribeUpdate | undefined> {
		return Message.decodeMaybe(r, (r) => SubscribeUpdate.#decode(r, version));
	}
}

export class Subscribe {
	id: bigint;
	broadcast: Path.Valid;
	track: string;
	priority: number;
	ordered: boolean;
	maxLatency: number;

	startGroup?: number;
	endGroup?: number;

	constructor(props: {
		id: bigint;
		broadcast: Path.Valid;
		track: string;
		priority: number;
		ordered?: boolean;
		maxLatency?: number;
		startGroup?: number;
		endGroup?: number;
	}) {
		this.id = props.id;
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.priority = props.priority;
		this.ordered = props.ordered ?? false;
		this.maxLatency = props.maxLatency ?? 0;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.id);
		await w.string(Path.encode(this.broadcast));
		await w.string(this.track);
		await w.u8(this.priority);

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				await w.bool(this.ordered);
				await w.u53(this.maxLatency);
				await w.u53(this.startGroup !== undefined ? this.startGroup + 1 : 0);
				await w.u53(this.endGroup !== undefined ? this.endGroup + 1 : 0);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Subscribe> {
		const id = await r.u62();
		const broadcast = Path.decode(await r.string());
		const track = await r.string();
		const priority = await r.u8();

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new Subscribe({ id, broadcast, track, priority });
			default: {
				const ordered = await r.bool();
				const maxLatency = await r.u53();
				const startGroup = await r.u53();
				const endGroup = await r.u53();
				return new Subscribe({
					id,
					broadcast,
					track,
					priority,
					ordered,
					maxLatency,
					startGroup: startGroup > 0 ? startGroup - 1 : undefined,
					endGroup: endGroup > 0 ? endGroup - 1 : undefined,
				});
			}
		}
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Subscribe> {
		return Message.decode(r, (r) => Subscribe.#decode(r, version));
	}
}

/**
 * Publisher's acknowledgement on the Subscribe Stream for drafts 01-04.
 *
 * Draft-05+ replaced this with implicit acceptance plus {@link SubscribeStart} /
 * {@link SubscribeEnd}; the immutable codec/timescale/cache moved to TRACK_INFO.
 */
export class SubscribeOk {
	priority: number;
	ordered: boolean;
	maxLatency: number;
	startGroup?: number;
	endGroup?: number;

	constructor({
		priority = 0,
		ordered = false,
		maxLatency = 0,
		startGroup = undefined,
		endGroup = undefined,
	}: {
		priority?: number;
		ordered?: boolean;
		maxLatency?: number;
		startGroup?: number;
		endGroup?: number;
	}) {
		this.priority = priority;
		this.ordered = ordered;
		this.maxLatency = maxLatency;
		this.startGroup = startGroup;
		this.endGroup = endGroup;
	}

	async #encode(w: Writer, version: Version) {
		switch (version) {
			case Version.DRAFT_02:
				// noop
				break;
			case Version.DRAFT_01:
				await w.u8(this.priority ?? 0);
				break;
			// Draft-05+ never sends SUBSCRIBE_OK, but keep the field layout matching
			// Draft-03/04 so a stray future use stays well-formed.
			default:
				await w.u8(this.priority);
				await w.bool(this.ordered);
				await w.u53(this.maxLatency);
				await w.u53(this.startGroup !== undefined ? this.startGroup + 1 : 0);
				await w.u53(this.endGroup !== undefined ? this.endGroup + 1 : 0);
				break;
		}
	}

	static async #decode(version: Version, r: Reader): Promise<SubscribeOk> {
		let priority: number | undefined;
		let ordered: boolean | undefined;
		let maxLatency: number | undefined;
		let startGroup: number | undefined;
		let endGroup: number | undefined;

		switch (version) {
			case Version.DRAFT_02:
				// noop
				break;
			case Version.DRAFT_01:
				priority = await r.u8();
				break;
			default:
				priority = await r.u8();
				ordered = await r.bool();
				maxLatency = await r.u53();
				startGroup = await r.u53();
				endGroup = await r.u53();
				break;
		}

		return new SubscribeOk({
			priority,
			ordered,
			maxLatency,
			startGroup: startGroup !== undefined && startGroup > 0 ? startGroup - 1 : undefined,
			endGroup: endGroup !== undefined && endGroup > 0 ? endGroup - 1 : undefined,
		});
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeOk> {
		return Message.decode(r, SubscribeOk.#decode.bind(SubscribeOk, version));
	}
}

/**
 * Resolves the absolute start group of a Draft-05+ subscription. The first message
 * the publisher sends, once the start group is known. A value greater than the
 * requested start implicitly drops the leading range.
 */
export class SubscribeStart {
	group: number;

	constructor(group: number) {
		this.group = group;
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, async (w) => {
			await w.u53(this.group);
		});
	}

	static async decode(r: Reader): Promise<SubscribeStart> {
		return Message.decode(r, async (r) => new SubscribeStart(await r.u53()));
	}
}

/**
 * Signals that no group at or after `group` (exclusive upper bound) will be produced
 * on a Draft-05+ subscription. `0` means the track ended before producing any groups.
 */
export class SubscribeEnd {
	/** The exclusive final group sequence: the first sequence that will never be produced. */
	group: number;

	constructor(group: number) {
		this.group = group;
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, async (w) => {
			await w.u53(this.group);
		});
	}

	static async decode(r: Reader): Promise<SubscribeEnd> {
		return Message.decode(r, async (r) => new SubscribeEnd(await r.u53()));
	}
}

/// Indicates that one or more groups have been dropped.
///
/// Draft03+ only.
export class SubscribeDrop {
	start: number;
	end: number;
	error: number;

	constructor(props: { start: number; end: number; error: number }) {
		this.start = props.start;
		this.end = props.end;
		this.error = props.error;
	}

	async #encode(w: Writer) {
		await w.u53(this.start);
		await w.u53(this.end);
		await w.u53(this.error);
	}

	static async #decode(r: Reader): Promise<SubscribeDrop> {
		return new SubscribeDrop({ start: await r.u53(), end: await r.u53(), error: await r.u53() });
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<SubscribeDrop> {
		return Message.decode(r, SubscribeDrop.#decode);
	}
}

/**
 * A response message on the subscribe stream, prefixed with a type discriminator
 * on Draft-03+.
 *
 * The discriminator is version-dependent:
 * - Draft-03/04: `0x0` SUBSCRIBE_OK, `0x1` SUBSCRIBE_DROP.
 * - Draft-05+: `0x0` SUBSCRIBE_START, `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP
 *   (SUBSCRIBE_OK was removed; acceptance is implicit).
 */
export type SubscribeResponse =
	| { ok: SubscribeOk }
	| { start: SubscribeStart }
	| { end: SubscribeEnd }
	| { drop: SubscribeDrop };

export async function encodeSubscribeResponse(w: Writer, resp: SubscribeResponse, version: Version): Promise<void> {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			if ("ok" in resp) {
				await resp.ok.encode(w, version);
			} else {
				throw new Error("only SUBSCRIBE_OK is supported for this version");
			}
			break;
		case Version.DRAFT_03:
		case Version.DRAFT_04:
			if ("ok" in resp) {
				await w.u53(0x0);
				await resp.ok.encode(w, version);
			} else if ("drop" in resp) {
				await w.u53(0x1);
				await resp.drop.encode(w);
			} else {
				throw new Error("SUBSCRIBE_START/END not supported for this version");
			}
			break;
		default:
			// Draft-05+: SUBSCRIBE_OK is gone; START/END/DROP carry the resolved range.
			if ("start" in resp) {
				await w.u53(0x0);
				await resp.start.encode(w);
			} else if ("end" in resp) {
				await w.u53(0x1);
				await resp.end.encode(w);
			} else if ("drop" in resp) {
				await w.u53(0x2);
				await resp.drop.encode(w);
			} else {
				throw new Error("SUBSCRIBE_OK not supported for this version");
			}
			break;
	}
}

export async function decodeSubscribeResponse(r: Reader, version: Version): Promise<SubscribeResponse> {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			return { ok: await SubscribeOk.decode(r, version) };
		case Version.DRAFT_03:
		case Version.DRAFT_04: {
			const typ = await r.u53();
			switch (typ) {
				case 0x0:
					return { ok: await SubscribeOk.decode(r, version) };
				case 0x1:
					return { drop: await SubscribeDrop.decode(r) };
				default:
					throw new Error(`unknown subscribe response type: ${typ}`);
			}
		}
		default: {
			const typ = await r.u53();
			switch (typ) {
				case 0x0:
					return { start: await SubscribeStart.decode(r) };
				case 0x1:
					return { end: await SubscribeEnd.decode(r) };
				case 0x2:
					return { drop: await SubscribeDrop.decode(r) };
				default:
					throw new Error(`unknown subscribe response type: ${typ}`);
			}
		}
	}
}

/** Like {@link decodeSubscribeResponse} but resolves `undefined` on a clean FIN. */
export async function decodeSubscribeResponseMaybe(
	r: Reader,
	version: Version,
): Promise<SubscribeResponse | undefined> {
	if (await r.done()) return undefined;
	return decodeSubscribeResponse(r, version);
}

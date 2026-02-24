import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { unreachable } from "../util/error.ts";
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
		this.ordered = props.ordered ?? true;
		this.maxLatency = props.maxLatency ?? 0;
		this.startGroup = props.startGroup;
		this.endGroup = props.endGroup;
	}

	async #encode(w: Writer, version: Version) {
		switch (version) {
			case Version.DRAFT_03:
				await w.u8(this.priority);
				await w.bool(this.ordered);
				await w.u53(this.maxLatency);
				await w.u53(this.startGroup !== undefined ? this.startGroup + 1 : 0);
				await w.u53(this.endGroup !== undefined ? this.endGroup + 1 : 0);
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				await w.u8(this.priority);
				break;
			default:
				unreachable(version);
		}
	}

	static async #decode(r: Reader, version: Version): Promise<SubscribeUpdate> {
		switch (version) {
			case Version.DRAFT_03: {
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
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new SubscribeUpdate({ priority: await r.u8() });
			default:
				unreachable(version);
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
		await w.string(this.broadcast);
		await w.string(this.track);
		await w.u8(this.priority);

		switch (version) {
			case Version.DRAFT_03:
				await w.bool(this.ordered);
				await w.u53(this.maxLatency);
				await w.u53(this.startGroup !== undefined ? this.startGroup + 1 : 0);
				await w.u53(this.endGroup !== undefined ? this.endGroup + 1 : 0);
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				unreachable(version);
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Subscribe> {
		const id = await r.u62();
		const broadcast = Path.from(await r.string());
		const track = await r.string();
		const priority = await r.u8();

		switch (version) {
			case Version.DRAFT_03: {
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
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new Subscribe({ id, broadcast, track, priority });
			default:
				unreachable(version);
		}
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Subscribe> {
		return Message.decode(r, (r) => Subscribe.#decode(r, version));
	}
}

export class SubscribeOk {
	priority: number;
	ordered: boolean;
	maxLatency: number;
	startGroup?: number;
	endGroup?: number;

	constructor({
		priority = 0,
		ordered = true,
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
			case Version.DRAFT_03:
				await w.u8(this.priority);
				await w.bool(this.ordered);
				await w.u53(this.maxLatency);
				await w.u53(this.startGroup !== undefined ? this.startGroup + 1 : 0);
				await w.u53(this.endGroup !== undefined ? this.endGroup + 1 : 0);
				break;
			case Version.DRAFT_02:
				// noop
				break;
			case Version.DRAFT_01:
				await w.u8(this.priority ?? 0);
				break;
			default:
				unreachable(version);
		}
	}

	static async #decode(version: Version, r: Reader): Promise<SubscribeOk> {
		let priority: number | undefined;
		let ordered: boolean | undefined;
		let maxLatency: number | undefined;
		let startGroup: number | undefined;
		let endGroup: number | undefined;

		switch (version) {
			case Version.DRAFT_03:
				priority = await r.u8();
				ordered = await r.bool();
				maxLatency = await r.u53();
				startGroup = await r.u53();
				endGroup = await r.u53();
				break;
			case Version.DRAFT_02:
				// noop
				break;
			case Version.DRAFT_01:
				priority = await r.u8();
				break;
			default:
				unreachable(version);
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

/// Indicates that one or more groups have been dropped.
///
/// Draft03 only.
export class SubscribeDrop {
	sequence: number;
	count: number;
	error: number;

	constructor(sequence: number, count: number, error: number) {
		this.sequence = sequence;
		this.count = count;
		this.error = error;
	}

	async #encode(w: Writer) {
		await w.u53(this.sequence);
		await w.u53(this.count);
		await w.u53(this.error);
	}

	static async #decode(r: Reader): Promise<SubscribeDrop> {
		return new SubscribeDrop(await r.u53(), await r.u53(), await r.u53());
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<SubscribeDrop> {
		return Message.decode(r, SubscribeDrop.#decode);
	}
}

/**
 * A response message on the subscribe stream.
 *
 * In Draft03, each response is prefixed with a type discriminator:
 * - 0x0 for SUBSCRIBE_OK
 * - 0x1 for SUBSCRIBE_DROP
 *
 * SUBSCRIBE_OK must be the first message on the response stream.
 */
export type SubscribeResponse = { ok: SubscribeOk } | { drop: SubscribeDrop };

export async function encodeSubscribeResponse(w: Writer, resp: SubscribeResponse, version: Version): Promise<void> {
	switch (version) {
		case Version.DRAFT_03:
			if ("ok" in resp) {
				await w.u53(0x0);
				await resp.ok.encode(w, version);
			} else {
				await w.u53(0x1);
				await resp.drop.encode(w);
			}
			break;
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			if ("ok" in resp) {
				await resp.ok.encode(w, version);
			} else {
				throw new Error("subscribe drop not supported for this version");
			}
			break;
		default:
			unreachable(version);
	}
}

export async function decodeSubscribeResponse(r: Reader, version: Version): Promise<SubscribeResponse> {
	switch (version) {
		case Version.DRAFT_03: {
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
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			return { ok: await SubscribeOk.decode(r, version) };
		default:
			unreachable(version);
	}
}

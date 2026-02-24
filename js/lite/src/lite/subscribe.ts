import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { unreachable } from "../util/error.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

export class SubscribeUpdate {
	priority: number;
	ordered: boolean;
	maxLatency: number;
	startGroup: number;
	endGroup: number;

	constructor(
		priority: number,
		ordered: boolean = true,
		maxLatency: number = 0,
		startGroup: number = 0,
		endGroup: number = 0,
	) {
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
				await w.u53(this.startGroup);
				await w.u53(this.endGroup);
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
				return new SubscribeUpdate(priority, ordered, maxLatency, startGroup, endGroup);
			}
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				return new SubscribeUpdate(await r.u8());
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

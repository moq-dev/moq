import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
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

	async #encode(w: Writer, version?: Version) {
		await w.u8(this.priority);
		if (version === Version.DRAFT_03) {
			await w.bool(this.ordered);
			await w.u53(this.maxLatency);
			await w.u53(this.startGroup);
			await w.u53(this.endGroup);
		}
	}

	static async #decode(r: Reader, version?: Version): Promise<SubscribeUpdate> {
		const priority = await r.u8();
		if (version === Version.DRAFT_03) {
			const ordered = await r.bool();
			const maxLatency = await r.u53();
			const startGroup = await r.u53();
			const endGroup = await r.u53();
			return new SubscribeUpdate(priority, ordered, maxLatency, startGroup, endGroup);
		}
		return new SubscribeUpdate(priority);
	}

	async encode(w: Writer, version?: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version?: Version): Promise<SubscribeUpdate> {
		return Message.decode(r, (r) => SubscribeUpdate.#decode(r, version));
	}

	static async decodeMaybe(r: Reader, version?: Version): Promise<SubscribeUpdate | undefined> {
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
	startGroup: number;
	endGroup: number;

	constructor(
		id: bigint,
		broadcast: Path.Valid,
		track: string,
		priority: number,
		ordered: boolean = true,
		maxLatency: number = 0,
		startGroup: number = 0,
		endGroup: number = 0,
	) {
		this.id = id;
		this.broadcast = broadcast;
		this.track = track;
		this.priority = priority;
		this.ordered = ordered;
		this.maxLatency = maxLatency;
		this.startGroup = startGroup;
		this.endGroup = endGroup;
	}

	async #encode(w: Writer, version?: Version) {
		await w.u62(this.id);
		await w.string(this.broadcast);
		await w.string(this.track);
		await w.u8(this.priority);
		if (version === Version.DRAFT_03) {
			await w.bool(this.ordered);
			await w.u53(this.maxLatency);
			await w.u53(this.startGroup);
			await w.u53(this.endGroup);
		}
	}

	static async #decode(r: Reader, version?: Version): Promise<Subscribe> {
		const id = await r.u62();
		const broadcast = Path.from(await r.string());
		const track = await r.string();
		const priority = await r.u8();
		if (version === Version.DRAFT_03) {
			const ordered = await r.bool();
			const maxLatency = await r.u53();
			const startGroup = await r.u53();
			const endGroup = await r.u53();
			return new Subscribe(id, broadcast, track, priority, ordered, maxLatency, startGroup, endGroup);
		}
		return new Subscribe(id, broadcast, track, priority);
	}

	async encode(w: Writer, version?: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version?: Version): Promise<Subscribe> {
		return Message.decode(r, (r) => Subscribe.#decode(r, version));
	}
}

export class SubscribeOk {
	// The version
	readonly version: Version;
	priority?: number;
	ordered?: boolean;
	maxLatency?: number;
	startGroup?: number;
	endGroup?: number;

	constructor({
		version,
		priority = undefined,
		ordered = undefined,
		maxLatency = undefined,
		startGroup = undefined,
		endGroup = undefined,
	}: {
		version: Version;
		priority?: number;
		ordered?: boolean;
		maxLatency?: number;
		startGroup?: number;
		endGroup?: number;
	}) {
		this.version = version;
		this.priority = priority;
		this.ordered = ordered;
		this.maxLatency = maxLatency;
		this.startGroup = startGroup;
		this.endGroup = endGroup;
	}

	async #encode(w: Writer) {
		if (this.version === Version.DRAFT_03) {
			await w.u8(this.priority ?? 0);
			await w.bool(this.ordered ?? true);
			await w.u53(this.maxLatency ?? 0);
			await w.u53(this.startGroup ?? 0);
			await w.u53(this.endGroup ?? 0);
		} else if (this.version === Version.DRAFT_02) {
			// noop
		} else if (this.version === Version.DRAFT_01) {
			await w.u8(this.priority ?? 0);
		}
	}

	static async #decode(version: Version, r: Reader): Promise<SubscribeOk> {
		let priority: number | undefined;
		let ordered: boolean | undefined;
		let maxLatency: number | undefined;
		let startGroup: number | undefined;
		let endGroup: number | undefined;

		if (version === Version.DRAFT_03) {
			priority = await r.u8();
			ordered = await r.bool();
			maxLatency = await r.u53();
			startGroup = await r.u53();
			endGroup = await r.u53();
		} else if (version === Version.DRAFT_02) {
			// noop
		} else if (version === Version.DRAFT_01) {
			priority = await r.u8();
		}

		return new SubscribeOk({ version, priority, ordered, maxLatency, startGroup, endGroup });
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeOk> {
		return Message.decode(r, SubscribeOk.#decode.bind(SubscribeOk, version));
	}
}

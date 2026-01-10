import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Time from "../time.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

export class SubscribeUpdate {
	priority: number;
	ordered: boolean;
	maxLatency: Time.Milli;

	constructor({ priority, maxLatency, ordered }: { priority: number; maxLatency: Time.Milli; ordered: boolean }) {
		this.priority = priority;
		this.maxLatency = maxLatency;
		this.ordered = ordered;
	}

	async #encode(w: Writer, version: Version) {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				await w.u8(this.priority);
				break;
			case Version.DRAFT_03:
				await w.u8(this.priority);
				await w.bool(this.ordered);
				await w.u53(this.maxLatency);
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}
	}

	static async #decode(r: Reader, version: Version): Promise<SubscribeUpdate> {
		let priority: number;
		let maxLatency = Time.Milli.zero;
		let ordered = false;

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				priority = await r.u8();
				break;
			case Version.DRAFT_03:
				priority = await r.u8();
				ordered = await r.bool();
				maxLatency = (await r.u53()) as Time.Milli;
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}

		return new SubscribeUpdate({ priority, maxLatency, ordered });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeUpdate> {
		return Message.decode(r, (r) => SubscribeUpdate.#decode(r, version));
	}
}

export class Subscribe {
	id: bigint;
	broadcast: Path.Valid;
	track: string;
	priority: number;
	ordered: boolean;
	maxLatency: Time.Milli;

	constructor({
		id,
		broadcast,
		track,
		priority,
		ordered,
		maxLatency,
	}: {
		id: bigint;
		broadcast: Path.Valid;
		track: string;
		priority: number;
		ordered: boolean;
		maxLatency: Time.Milli;
	}) {
		this.id = id;
		this.broadcast = broadcast;
		this.track = track;
		this.priority = priority;
		this.ordered = ordered;
		this.maxLatency = maxLatency;
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
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Subscribe> {
		const id = await r.u62();
		const broadcast = Path.from(await r.string());
		const track = await r.string();
		const priority = await r.u8();
		let maxLatency = Time.Milli.zero;
		let ordered = false;

		switch (version) {
			case Version.DRAFT_03:
				ordered = await r.bool();
				maxLatency = (await r.u53()) as Time.Milli;
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}

		return new Subscribe({ id, broadcast, track, priority, maxLatency, ordered });
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
	maxLatency: Time.Milli;

	constructor({ priority, maxLatency, ordered }: { priority: number; maxLatency: Time.Milli; ordered: boolean }) {
		this.priority = priority;
		this.maxLatency = maxLatency;
		this.ordered = ordered;
	}

	async #encode(version: Version, w: Writer) {
		switch (version) {
			case Version.DRAFT_01:
				await w.u8(this.priority);
				break;
			case Version.DRAFT_02:
				break;
			case Version.DRAFT_03:
				await w.u8(this.priority);
				await w.bool(this.ordered);
				await w.u53(this.maxLatency);
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}
	}

	static async #decode(version: Version, r: Reader): Promise<SubscribeOk> {
		let priority = 0;
		let ordered = false;
		let maxLatency = Time.Milli.zero;

		switch (version) {
			case Version.DRAFT_01:
				priority = await r.u8();
				break;
			case Version.DRAFT_02:
				// noop
				break;
			case Version.DRAFT_03:
				priority = await r.u8();
				ordered = await r.bool();
				maxLatency = (await r.u53()) as Time.Milli;
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}

		return new SubscribeOk({ priority, maxLatency, ordered });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, this.#encode.bind(this, version));
	}

	static async decode(r: Reader, version: Version): Promise<SubscribeOk> {
		return Message.decode(r, (r) => SubscribeOk.#decode(version, r));
	}
}

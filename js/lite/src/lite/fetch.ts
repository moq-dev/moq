import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";

export class Fetch {
	broadcast: Path.Valid;
	track: string;
	priority: number;
	group: number;

	constructor(broadcast: Path.Valid, track: string, priority: number, group: number) {
		this.broadcast = broadcast;
		this.track = track;
		this.priority = priority;
		this.group = group;
	}

	async #encode(w: Writer) {
		await w.string(this.broadcast);
		await w.string(this.track);
		await w.u8(this.priority);
		await w.u53(this.group);
	}

	static async #decode(r: Reader): Promise<Fetch> {
		const broadcast = Path.from(await r.string());
		const track = await r.string();
		const priority = await r.u8();
		const group = await r.u53();
		return new Fetch(broadcast, track, priority, group);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<Fetch> {
		return Message.decode(r, Fetch.#decode);
	}
}

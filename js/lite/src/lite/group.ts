import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";

export class Group {
	subscribe: bigint;
	sequence: number;

	constructor(subscribe: bigint, sequence: number) {
		this.subscribe = subscribe;
		this.sequence = sequence;
	}

	async #encode(w: Writer) {
		await w.u62(this.subscribe);
		await w.u53(this.sequence);
	}

	static async #decode(r: Reader): Promise<Group> {
		return new Group(await r.u62(), await r.u53());
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<Group> {
		return Message.decode(r, Group.#decode);
	}
}

export class GroupDrop {
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

	static async #decode(r: Reader): Promise<GroupDrop> {
		return new GroupDrop(await r.u53(), await r.u53(), await r.u53());
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<GroupDrop> {
		return Message.decode(r, GroupDrop.#decode);
	}
}

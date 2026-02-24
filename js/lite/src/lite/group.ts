import type { Reader, Writer } from "../stream.ts";
import { unreachable } from "../util/error.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

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

	static async decodeMaybe(r: Reader): Promise<Group | undefined> {
		return Message.decodeMaybe(r, Group.#decode);
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

	static #guard(version: Version) {
		switch (version) {
			case Version.DRAFT_03:
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				throw new Error("group drop not supported for this version");
			default:
				unreachable(version);
		}
	}

	async #encode(w: Writer) {
		await w.u53(this.sequence);
		await w.u53(this.count);
		await w.u53(this.error);
	}

	static async #decode(r: Reader): Promise<GroupDrop> {
		return new GroupDrop(await r.u53(), await r.u53(), await r.u53());
	}

	async encode(w: Writer, version: Version): Promise<void> {
		GroupDrop.#guard(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<GroupDrop> {
		GroupDrop.#guard(version);
		return Message.decode(r, GroupDrop.#decode);
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<GroupDrop | undefined> {
		GroupDrop.#guard(version);
		return Message.decodeMaybe(r, GroupDrop.#decode);
	}
}

export class Frame {
	payload: Uint8Array;

	constructor(payload: Uint8Array) {
		this.payload = payload;
	}

	async #encode(w: Writer) {
		await w.write(this.payload);
	}

	static async #decode(r: Reader): Promise<Frame> {
		const payload = await r.readAll();
		return new Frame(payload);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<Frame> {
		return Message.decode(r, Frame.#decode);
	}
}

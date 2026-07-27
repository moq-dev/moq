import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { hasFrameBounds, type Version } from "./version.ts";

export class Group {
	subscribe: bigint;
	sequence: number;

	/**
	 * The index of the first frame on this stream within the group.
	 *
	 * 0 (the common case) means the stream carries the group from its beginning. A
	 * higher value means the leading frames are not here, either because the
	 * subscription started partway into the group or because the publisher only holds
	 * the tail. Lite-06+; older versions always start at 0.
	 */
	frameStart: number;

	constructor(subscribe: bigint, sequence: number, frameStart = 0) {
		this.subscribe = subscribe;
		this.sequence = sequence;
		this.frameStart = frameStart;
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.subscribe);
		await w.u53(this.sequence);
		if (hasFrameBounds(version)) {
			await w.u53(this.frameStart);
		} else if (this.frameStart !== 0) {
			// The peer would number the frames from 0 and silently misalign the group.
			throw new Error("frame offsets not supported for this version");
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Group> {
		const subscribe = await r.u62();
		const sequence = await r.u53();
		const frameStart = hasFrameBounds(version) ? await r.u53() : 0;
		return new Group(subscribe, sequence, frameStart);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Group> {
		return Message.decode(r, (r) => Group.#decode(r, version));
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<Group | undefined> {
		return Message.decodeMaybe(r, (r) => Group.#decode(r, version));
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

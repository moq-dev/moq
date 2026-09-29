import { race } from "@moq/signals";
import type * as netGroup from "../group.ts";
import type { Cursor, Reader, Writer } from "../stream.ts";
import * as Time from "../time.ts";
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

	constructor({
		subscribe,
		sequence,
		frameStart = 0,
	}: {
		subscribe: bigint;
		sequence: number;
		frameStart?: number;
	}) {
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
		return new Group({ subscribe, sequence, frameStart });
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

/** Decode an unsigned zigzag varint back to a signed delta (mirrors Rust `VarInt::to_zigzag`). */
function unzigzag(v: bigint): bigint {
	return (v >> 1n) ^ -(v & 1n);
}

/**
 * A synchronous decode for one frame of a group or FETCH response stream.
 *
 * A non-zero `scale` means every frame is prefixed with a zigzag-delta timestamp (the lite-05
 * FRAME format), decoded into a Timestamp at that scale. Scale 0 (pre-lite-05) carries no
 * timestamp, so frames are wall-clock stamped on arrival.
 */
export function frameDecoder(scale: number): (c: Cursor) => netGroup.Frame {
	if (scale === 0) {
		return (c) => ({ payload: c.read(c.u53()), timestamp: Time.Timestamp.now() });
	}

	const timescale = Time.Timescale(scale);
	let prevTs = 0n;
	return (c) => {
		const delta = unzigzag(c.u62());
		const payload = c.read(c.u53());
		// After the last read, so a decode that ran short and gets retried adds the delta once.
		prevTs += delta;
		return { payload, timestamp: new Time.Timestamp(Number(prevTs), timescale) };
	};
}

/** Write a group stream's frames into `producer` until the stream ends or the producer closes. */
export async function readFrames(stream: Reader, producer: netGroup.Producer, scale: number): Promise<void> {
	const decode = frameDecoder(scale);

	for (;;) {
		// Every frame already buffered is written without an await, so the reader wakes once
		// per batch rather than once per frame. Only the group's own stream ends it: a track
		// that closes first has already closed (or aborted) this group through its cache.
		const frame = stream.tryDecode(decode) ?? (await race([stream.decodeMaybe(decode), producer.closed]));
		if (!frame || frame instanceof Error) return;
		producer.writeFrame(frame);
	}
}

import { race } from "@moq/signals";
import { FrameTooLarge } from "../error.ts";
import * as netGroup from "../group.ts";
import { hooks } from "../internal.ts";
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

/** Decode an unsigned zigzag varint back to a signed delta (mirrors Rust `varint::unzigzag`). */
function unzigzag(v: bigint): bigint {
	return (v >> 1n) ^ -(v & 1n);
}

/** A decoded frame whose payload may still be a stored slice. */
export interface WireFrame extends netGroup.Frame {
	/** The declared plain size on a compressed track. */
	inflated?: number;
}

/** How {@link frameDecoder} reads each frame's optional fields. */
export interface FrameFormat {
	/** The wire timescale; non-zero means every frame carries a zigzag-delta timestamp. */
	scale: number;
	/** Whether every frame declares its plain size ahead of a stored slice. */
	compress?: boolean;
}

/** Decode one frame, with timestamp deltas when scaled and declared plain sizes when compressed. */
export function frameDecoder({ scale, compress = false }: FrameFormat): (c: Cursor) => WireFrame {
	const timescale = scale === 0 ? undefined : Time.Timescale(scale);
	let prevTs = 0n;
	return (c) => {
		const delta = timescale === undefined ? 0n : unzigzag(c.u62());
		const inflated = compress ? c.u53() : undefined;
		const size = c.u53();
		if (size > netGroup.MAX_GROUP_CACHE_BYTES || (inflated ?? 0) > netGroup.MAX_GROUP_CACHE_BYTES)
			throw new FrameTooLarge();
		const payload = c.read(size);
		// Commit the delta only after the last read: an incomplete decode gets retried.
		prevTs += delta;
		const timestamp = timescale === undefined ? undefined : new Time.Timestamp(Number(prevTs), timescale);
		return { payload, inflated, timestamp };
	};
}

/** Write a group stream's frames into `producer` until the stream ends or the producer closes. */
export async function readFrames(stream: Reader, producer: netGroup.Producer, format: FrameFormat): Promise<void> {
	const decode = frameDecoder(format);

	for (;;) {
		// Every frame already buffered is written without an await, so the reader wakes once
		// per batch rather than once per frame. Only the group's own stream ends it: a track
		// that closes first has already closed (or aborted) this group through its cache.
		const frame = stream.tryDecode(decode) ?? (await race([stream.decodeMaybe(decode), producer.closed]));
		if (!frame || frame instanceof Error) return;
		hooks.writeGroupFrame(producer, frame, frame.inflated);
	}
}

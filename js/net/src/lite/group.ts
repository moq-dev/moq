import { race } from "@moq/signals";
import { FrameTooLarge, StreamCode, StreamError } from "../error.ts";
import type { Producer as FrameProducer } from "../frame.ts";
import type * as netGroup from "../group.ts";
import { hooks } from "../internal.ts";
import { decodePayload, fillFrame, receiveFrame, skipPayload, type WirePayload } from "../payload.ts";
import type { Cursor, Reader, Writer } from "../stream.ts";
import * as Time from "../time.ts";
import { toNumber } from "../util/bigint.ts";
import { MAX_FRAME_BYTES } from "../util/limits.ts";
import * as Message from "./message.ts";
import { hasFrameBounds, type Version } from "./version.ts";

export class Group {
	subscribe: bigint;
	sequence: bigint;

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
		sequence: bigint;
		frameStart?: number;
	}) {
		this.subscribe = subscribe;
		this.sequence = sequence;
		this.frameStart = frameStart;
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.subscribe);
		await w.u62(this.sequence);
		if (hasFrameBounds(version)) {
			await w.u53(this.frameStart);
		} else if (this.frameStart !== 0) {
			// The peer would number the frames from 0 and silently misalign the group.
			throw new Error("frame offsets not supported for this version");
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Group> {
		const subscribe = await r.u62();
		const sequence = await r.u62();
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

/**
 * A malformed FRAME on a Group Stream, which ends only that stream while the session and its other
 * streams go on (lite-07). Mirrors the Rust `UniState::Group`, which aborts only the stream.
 */
function malformed(message: string): StreamError {
	return new StreamError(StreamCode.MalformedTrack, { message });
}

/** Decode an unsigned zigzag varint back to a signed delta (mirrors Rust `varint::unzigzag`). */
function unzigzag(v: bigint): bigint {
	return (v >> 1n) ^ -(v & 1n);
}

/** A decoded frame, whose payload may still be a stored slice or follow on the stream. */
export interface WireFrame extends WirePayload {
	/** The declared plain size on a compressed track. */
	inflated?: number;
}

/** How {@link frameDecoder} reads each frame's optional fields. */
export interface FrameFormat {
	/** The wire timescale; non-zero means every frame carries a zigzag-delta timestamp. */
	scale: number;
	/** Whether every frame declares its plain size ahead of a stored slice. */
	compress?: boolean;
	/**
	 * Whether every frame leads with its `Payload Length`, the payload size plus one (lite-07),
	 * rather than ending its header with the size.
	 */
	lengthFirst?: boolean;
}

/**
 * Decode one frame, with timestamp deltas when scaled and declared plain sizes when compressed.
 * Lite-07 leads with the size plus one, where stream end ends the group; before it the size ends
 * the header, and stream end at the frame's first field ends the group.
 */
export function frameDecoder({ scale, compress = false, lengthFirst = false }: FrameFormat): (c: Cursor) => WireFrame {
	const timescale = scale === 0 ? undefined : Time.Timescale(scale);
	let prevTs = 0n;
	return (c) => {
		let size = 0;
		if (lengthFirst) {
			const length = c.u53();
			// 0 is reserved for GROUP_END, which no version defines yet.
			if (length === 0) throw malformed("FRAME payload length of 0");
			size = length - 1;
		}
		const delta = timescale === undefined ? 0n : unzigzag(c.u62());
		const inflated = compress ? c.u53() : undefined;
		if (!lengthFirst) size = c.u53();
		// A compressed frame is stored whole; a plain one past a segment streams in after this.
		if (compress && (size > MAX_FRAME_BYTES || (inflated ?? 0) > MAX_FRAME_BYTES)) throw new FrameTooLarge();
		const payload = compress ? c.read(size) : decodePayload(c, size);
		// Commit the delta only after the last read: an incomplete decode gets retried.
		prevTs += delta;
		const timestamp = timescale === undefined ? undefined : new Time.Timestamp(toNumber(prevTs), timescale);
		return { size, payload, inflated, timestamp };
	};
}

/**
 * Where a lite-07 Group Stream is in its group, which a YIELDED reset pauses and a later stream
 * continues: frames the group holds already are skipped, and the frame the reset cut short is
 * completed. Mirrors the Rust `FrameIngest` on a yielding stream.
 *
 * @internal
 */
export interface Ingest {
	/** The group index of the next frame on the stream, counted from its `Frame Start`. */
	index: number;
	/**
	 * The frame a YIELDED reset cut short, holding the group's writer: kept on a pause, and
	 * completed by the continuation.
	 */
	partial?: FrameProducer;
}

/**
 * Write a group stream's frames into `producer` until the stream ends or the producer closes.
 *
 * With `ingest`, a stream continuing a paused group skips the frames the group holds, and
 * completes the one a YIELDED reset cut short; ending before completing it is malformed.
 */
export async function readFrames(
	stream: Reader,
	producer: netGroup.Producer,
	format: FrameFormat,
	ingest?: Ingest,
): Promise<void> {
	const decode = frameDecoder(format);

	for (;;) {
		// Every frame already buffered is written without an await, so the reader wakes once
		// per batch rather than once per frame. Only the group's own stream ends it: a track
		// that closes first has already closed (or aborted) this group through its cache.
		const frame = stream.tryDecode(decode) ?? (await race([stream.decodeMaybe(decode), producer.closed]));
		if (frame === undefined && ingest?.partial)
			throw malformed("a continuation ended before completing the frame it resumed");
		if (!frame || frame instanceof Error) return;
		if (ingest) {
			const index = ingest.index++;
			const held = hooks.groupFrameCount(producer);
			if (index < held) {
				if (!(await skipHeld(stream, producer, frame, ingest, index + 1 === held))) return;
				continue;
			}
		}
		if (frame.payload)
			hooks.writeGroupFrame(
				producer,
				{ payload: frame.payload, timestamp: Time.received(frame.timestamp, producer.sequence) },
				frame.inflated,
			);
		else if (ingest) {
			const timestamp = Time.received(frame.timestamp, producer.sequence);
			const open = producer.createFrame({ size: frame.size, timestamp });
			// Kept until it completes, so a YIELDED reset cutting it short leaves it for the continuation.
			ingest.partial = open;
			if (!(await fillFrame(stream, producer, open))) return;
			ingest.partial = undefined;
		} else if (!(await receiveFrame(stream, producer, frame))) return;
	}
}

// A continuation re-sending a frame the group holds: discard it, or for the frame a YIELDED reset
// cut short (the `last` one held), discard the bytes held and complete it. Resolves false if the
// group closed first.
async function skipHeld(
	stream: Reader,
	producer: netGroup.Producer,
	frame: WireFrame,
	ingest: Ingest,
	last: boolean,
): Promise<boolean> {
	const partial = last ? ingest.partial : undefined;
	if (!partial) return frame.payload !== undefined || (await skipUnlessClosed(stream, producer, frame.size));
	// The same frame, so the same size: a publisher reusing the group's identity is refused
	// rather than written past.
	if (partial.size !== frame.size) throw malformed("a continuation resent a frame at another size");
	const skip = partial.size - partial.remaining;
	if (frame.payload) {
		partial.write(frame.payload.subarray(skip));
		partial.finish();
	} else {
		if (!(await skipUnlessClosed(stream, producer, skip))) return false;
		if (!(await fillFrame(stream, producer, partial))) return false;
	}
	ingest.partial = undefined;
	return true;
}

// Read past `size` bytes of a held payload, as `fillFrame` reads one in: resolves false if the group
// closed first, so a peer that stops sending holds neither the stream nor its task.
async function skipUnlessClosed(stream: Reader, producer: netGroup.Producer, size: number): Promise<boolean> {
	await race([skipPayload(stream, size), producer.closed]);
	return producer.closed.peek() === undefined;
}

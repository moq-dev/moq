import { race } from "@moq/signals";
import { FrameTooLarge, StreamCode, StreamError } from "../error.ts";
import type * as netGroup from "../group.ts";
import { hooks } from "../internal.ts";
import { decodePayload, receiveFrame, type WirePayload } from "../payload.ts";
import type { Cursor, Reader, Writer } from "../stream.ts";
import * as Time from "../time.ts";
import { toNumber } from "../util/bigint.ts";
import { RESERVED } from "../util/key.ts";
import { MAX_FRAME_BYTES } from "../util/limits.ts";
import * as Message from "./message.ts";
import { hasFrameBounds, type Version } from "./version.ts";

export class Group {
	subscribe: bigint;
	key: bigint;

	/**
	 * The index of the first frame on this stream within the group.
	 *
	 * 0 (the common case) means the stream carries the group from its beginning. A
	 * higher value means the leading frames are not here, because the subscription started
	 * partway into the group. Lite-06+ only; older versions always start at 0.
	 */
	frameStart: number;

	constructor({
		subscribe,
		key: sequence,
		frameStart = 0,
	}: {
		subscribe: bigint;
		key: bigint;
		frameStart?: number;
	}) {
		this.subscribe = subscribe;
		this.key = sequence;
		this.frameStart = frameStart;
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.subscribe);
		await w.u62(this.key);
		if (hasFrameBounds(version)) {
			await w.u53(this.frameStart);
		} else if (this.frameStart !== 0) {
			// The peer would number the frames from 0 and silently misalign the group.
			throw new Error("frame offsets not supported for this version");
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Group> {
		const subscribe = await r.u62();
		const key = await r.u62();
		const frameStart = hasFrameBounds(version) ? await r.u53() : 0;
		return new Group({ subscribe, key: key, frameStart });
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
	/** A metadata FRAME: its header alone, `size` being the plain size, with no payload. */
	header?: boolean;
}

/** Lite-07 GROUP_END: the group is whole, ending at `end` in the track's timescale. */
export interface GroupEnd {
	end: bigint;
}

/** How {@link frameDecoder} reads each frame's optional fields. */
export interface FrameFormat {
	/** The wire timescale; non-zero means every frame carries a delta timestamp. */
	scale: number;
	/**
	 * The group's start: lite-07's frame 0 sits there, a later stream's first timestamp delta counts
	 * from it, and so does a GROUP_END's End when the stream carries no timestamp. Before lite-07
	 * the first delta counts from 0.
	 */
	start?: bigint;
	/**
	 * The index of the stream's first frame, from its GROUP header: on lite-07, frame 0 carries no
	 * delta, and a GROUP_END before any frame is malformed only for a stream from frame 0.
	 * {@link readFrames} defaults it to where its group begins.
	 */
	frameStart?: number;
	/** Whether every frame declares its plain size ahead of a stored slice. */
	compress?: boolean;
	/**
	 * Whether every frame leads with its `Payload Length`, the payload size plus one (lite-07),
	 * rather than ending its header with the size. Lite-07 also carries unsigned timestamp deltas,
	 * where earlier versions zigzag a signed one.
	 */
	lengthFirst?: boolean;
	/**
	 * Whether every frame is its header alone, as a metadata subscription delivers it (lite-07): its
	 * `Payload Length` is the plain size plus one, with no Inflated Size and no payload, compressed
	 * track or not. {@link readFrames} takes it from the group it writes.
	 */
	meta?: boolean;
}

/**
 * Decode one frame, with timestamp deltas when stamped and declared plain sizes when compressed,
 * or a lite-07 GROUP_END. Lite-07 leads with the size plus one, 0 for GROUP_END, where stream end
 * closes the group; before it the size ends the header, and stream end at the frame's first field
 * ends the group.
 *
 * On lite-07 frame 0 carries no delta, sitting at the group's `start`; every other delta is an
 * unsigned offset from the prior frame, or from `start` for a stream that begins later, and
 * GROUP_END's End counts from the last frame's timestamp, or from `start` when the stream carried
 * none. Before lite-07 every delta is zigzagged, the first from 0. Mirrors the Rust `FrameIngest`.
 */
export function frameDecoder({
	scale,
	start = 0n,
	frameStart = 0,
	compress = false,
	lengthFirst = false,
	meta = false,
}: FrameFormat): (c: Cursor) => WireFrame | GroupEnd {
	if (meta && !lengthFirst) throw new Error("a metadata Group Stream needs moq-lite-07");
	const timescale = scale === 0 ? undefined : Time.Timescale(scale);
	let prevTs = lengthFirst ? start : 0n;
	// Lite-07's frame 0 sits at the group's start, so it carries no delta.
	let implicit = lengthFirst && frameStart === 0;
	return (c) => {
		let size = 0;
		if (lengthFirst) {
			const length = c.u53();
			if (length === 0) {
				// A group starts at its first frame, so a stream from frame 0 carries one.
				if (implicit) throw malformed("GROUP_END before frame 0");
				const end = prevTs + c.u62();
				if (end > RESERVED) throw malformed(`GROUP_END at ${end}, past the largest group end`);
				return { end };
			}
			size = length - 1;
		}
		const delta =
			timescale === undefined || implicit ? 0n : lengthFirst ? c.u62() : unzigzag(c.u62());
		const inflated = compress && !meta ? c.u53() : undefined;
		if (!lengthFirst) size = c.u53();
		// A compressed frame is stored whole; a plain one past a segment streams in after this,
		// and `decodePayload` caps its size. A metadata FRAME has no payload to read.
		if (compress && !meta && (size > MAX_FRAME_BYTES || (inflated ?? 0) > MAX_FRAME_BYTES))
			throw new FrameTooLarge();
		const payload = meta ? undefined : compress ? c.read(size) : decodePayload(c, size);
		const header = meta || undefined;
		// Commit the delta only after the last read: an incomplete decode gets retried.
		implicit = false;
		if (timescale === undefined) return { size, payload, inflated, header };
		prevTs += delta;
		return { size, payload, inflated, header, timestamp: new Time.Timestamp(toNumber(prevTs), timescale) };
	};
}

/**
 * Write a group stream's frames into `producer` until the stream ends or the producer closes.
 * Resolves with where a lite-07 GROUP_END said the group ends, or undefined when the stream ended
 * without one: the caller finishes the group at that end, and a bare FIN only closes it. Anything
 * after GROUP_END is malformed.
 */
export async function readFrames(
	stream: Reader,
	producer: netGroup.Producer,
	format: FrameFormat,
): Promise<bigint | undefined> {
	const decode = frameDecoder({
		...format,
		start: producer.start,
		frameStart: format.frameStart ?? hooks.groupOffset(producer),
		meta: hooks.groupMeta(producer),
	});

	for (;;) {
		// Every frame already buffered is written without an await, so the reader wakes once
		// per batch rather than once per frame. Only the group's own stream ends it: a track
		// that closes first has already closed (or aborted) this group through its cache.
		const frame = stream.tryDecode(decode) ?? (await race([stream.decodeMaybe(decode), producer.closed]));
		if (!frame || frame instanceof Error) return undefined;
		if ("end" in frame) {
			// The FIN may never come, so the group's own close, such as an abort or eviction, still
			// ends the wait.
			const done = await race([stream.done(), producer.closed]);
			if (done === false) throw malformed("bytes after GROUP_END");
			if (done !== true) return undefined;
			return frame.end;
		}
		if (frame.header) hooks.writeGroupHeader(producer, Time.received(frame.timestamp, producer.start), frame.size);
		else if (frame.payload)
			hooks.writeGroupFrame(
				producer,
				{ payload: frame.payload, timestamp: Time.received(frame.timestamp, producer.start) },
				frame.inflated,
			);
		else if (!(await receiveFrame(stream, producer, frame))) return;
	}
}

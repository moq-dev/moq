import { race } from "@moq/signals";
import { FrameTooLarge, StreamCode, StreamError, TimestampMismatch } from "../error.ts";
import type * as netGroup from "../group.ts";
import { hooks } from "../internal.ts";
import { decodePayload, receiveFrame, type WirePayload } from "../payload.ts";
import type { Cursor, Reader, Writer } from "../stream.ts";
import * as Time from "../time.ts";
import { toNumber } from "../util/bigint.ts";
import { MAX_FRAME_BYTES } from "../util/limits.ts";
import * as Message from "./message.ts";
import { hasGroupEnd, hasGroupFrameStart, type Version } from "./version.ts";

export class Group {
	subscribe: bigint;
	sequence: bigint;

	/**
	 * Lite-07: the start of the group before this one on the track, or undefined when the
	 * publisher does not know it. No group starts strictly between the two, so the starts between
	 * are skipped. A receiver refuses one at or above `sequence` as a malformed track.
	 */
	previous?: bigint;

	/**
	 * The index of the first frame on this stream within the group.
	 *
	 * 0 (the common case) means the stream carries the group from its beginning. A
	 * higher value means the leading frames are not here, because the subscription started
	 * partway into the group. Lite-06 only; other versions always start at 0.
	 */
	frameStart: number;

	constructor({
		subscribe,
		sequence,
		previous,
		frameStart = 0,
	}: {
		subscribe: bigint;
		sequence: bigint;
		previous?: bigint;
		frameStart?: number;
	}) {
		this.subscribe = subscribe;
		this.sequence = sequence;
		this.previous = previous;
		this.frameStart = frameStart;
	}

	/**
	 * The starts this header passes over as skipped, `[start, end)`: those strictly between the
	 * previous group and this one, empty when the previous group is unknown or just below.
	 */
	passedOver(): { start: bigint; end: bigint } {
		return passedOver(this.previous, this.sequence);
	}

	async #encode(w: Writer, version: Version) {
		await w.u62(this.subscribe);
		await w.u62(this.sequence);
		// Plus one, so 0 says unknown.
		if (hasGroupEnd(version)) await w.u62(this.previous === undefined ? 0n : this.previous + 1n);
		if (hasGroupFrameStart(version)) {
			await w.u53(this.frameStart);
		} else if (this.frameStart !== 0) {
			// The peer would number the frames from 0 and silently misalign the group.
			throw new Error("frame offsets not supported for this version");
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Group> {
		const subscribe = await r.u62();
		const sequence = await r.u62();
		const plusOne = hasGroupEnd(version) ? await r.u62() : 0n;
		const previous = plusOne === 0n ? undefined : plusOne - 1n;
		const frameStart = hasGroupFrameStart(version) ? await r.u53() : 0;
		return new Group({ subscribe, sequence, previous, frameStart });
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
 * The starts a GROUP for `start` passes over as skipped when its previous group starts at
 * `previous`: those strictly between, `[start, end)`. Empty when the previous group is unknown or
 * just below. Mirrors the Rust `lite::passed_over`.
 */
export function passedOver(previous: bigint | undefined, start: bigint): { start: bigint; end: bigint } {
	const from = previous === undefined ? start : previous + 1n;
	return { start: from < start ? from : start, end: start };
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

/** Lite-07 GROUP_END: the group is whole, ending at `end` in the track's timescale. */
export interface GroupEnd {
	end: bigint;
}

/** How {@link frameDecoder} reads each frame's optional fields. */
export interface FrameFormat {
	/**
	 * The wire timescale; non-zero means every frame carries a zigzag-delta timestamp. A lite-07
	 * frame carries one on a scale-less track too, which must be its group's `start`.
	 */
	scale: number;
	/** The group's start, which every frame on a scale-less lite-07 track is stamped with. */
	start?: bigint;
	/** Whether every frame declares its plain size ahead of a stored slice. */
	compress?: boolean;
	/**
	 * Whether every frame leads with its `Payload Length`, the payload size plus one (lite-07),
	 * rather than ending its header with the size.
	 */
	lengthFirst?: boolean;
}

/**
 * Decode one frame, with timestamp deltas when stamped and declared plain sizes when compressed,
 * or a lite-07 GROUP_END. Lite-07 leads with the size plus one, 0 for GROUP_END, where stream end
 * closes the group; before it the size ends the header, and stream end at the frame's first field
 * ends the group.
 */
export function frameDecoder({
	scale,
	start,
	compress = false,
	lengthFirst = false,
}: FrameFormat): (c: Cursor) => WireFrame | GroupEnd {
	const timescale = scale === 0 ? undefined : Time.Timescale(scale);
	// Lite-07 stamps a scale-less track's frames too, with their group's start.
	const stamped = timescale !== undefined || lengthFirst;
	let prevTs = 0n;
	return (c) => {
		let size = 0;
		if (lengthFirst) {
			const length = c.u53();
			if (length === 0) return { end: c.u62() };
			size = length - 1;
		}
		const delta = stamped ? unzigzag(c.u62()) : 0n;
		const inflated = compress ? c.u53() : undefined;
		if (!lengthFirst) size = c.u53();
		// A compressed frame is stored whole; a plain one past a segment streams in after this.
		if (compress && (size > MAX_FRAME_BYTES || (inflated ?? 0) > MAX_FRAME_BYTES)) throw new FrameTooLarge();
		const payload = compress ? c.read(size) : decodePayload(c, size);
		// Commit the delta only after the last read: an incomplete decode gets retried.
		prevTs += delta;
		// A scale-less frame's timestamp is its group's start, which the group passes on.
		if (timescale === undefined && stamped && prevTs !== start) throw new TimestampMismatch();
		const timestamp = timescale === undefined ? undefined : new Time.Timestamp(toNumber(prevTs), timescale);
		return { size, payload, inflated, timestamp };
	};
}

/**
 * Write a group stream's frames into `producer` until the stream ends or the producer closes.
 * Resolves with where a lite-07 GROUP_END said the group ends, or undefined when the stream ended
 * without one: the caller finishes the group at that end, and a bare FIN only closes it. Anything
 * after GROUP_END, or an End the group refuses (at or before its start), is malformed.
 */
export async function readFrames(
	stream: Reader,
	producer: netGroup.Producer,
	format: FrameFormat,
): Promise<bigint | undefined> {
	const decode = frameDecoder({ ...format, start: producer.sequence });

	for (;;) {
		// Every frame already buffered is written without an await, so the reader wakes once
		// per batch rather than once per frame. Only the group's own stream ends it: a track
		// that closes first has already closed (or aborted) this group through its cache.
		const frame = stream.tryDecode(decode) ?? (await race([stream.decodeMaybe(decode), producer.closed]));
		if (!frame || frame instanceof Error) return undefined;
		if ("end" in frame) {
			if (frame.end <= producer.sequence)
				throw malformed(`GROUP_END at ${frame.end}, not past the group's start`);
			// The FIN may never come, so the group's own close, such as an abort or eviction, still
			// ends the wait.
			const done = await race([stream.done(), producer.closed]);
			if (done === false) throw malformed("bytes after GROUP_END");
			if (done !== true) return undefined;
			return frame.end;
		}
		if (frame.payload)
			hooks.writeGroupFrame(
				producer,
				{ payload: frame.payload, timestamp: Time.received(frame.timestamp, producer.sequence) },
				frame.inflated,
			);
		else if (!(await receiveFrame(stream, producer, frame))) return;
	}
}

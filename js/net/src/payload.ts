/**
 * A frame payload's way between a wire stream and a group. A payload within one segment is
 * decoded with its header; a larger one streams through in chunks, so no hop holds it whole.
 *
 * @module
 */
import { race } from "@moq/signals";
import type * as group from "./group.ts";
import { hooks, type ReadGroupFrame } from "./internal.ts";
import type { Cursor, Reader } from "./stream.ts";
import type { Timestamp } from "./time.ts";
import { SEGMENT_BYTES } from "./util/limits.ts";

/** A decoded frame header, and its payload unless that streams in after it. */
export interface WirePayload {
	/** The payload size in bytes. */
	size: number;
	/** The payload, or `undefined` past one segment, where it follows the header on the stream. */
	payload?: Uint8Array;
	timestamp?: Timestamp;
}

/** Read a `size`-byte payload with its header when it fits one segment; a larger one is left on the stream. */
export function decodePayload(c: Cursor, size: number): Uint8Array | undefined {
	return size > SEGMENT_BYTES ? undefined : c.read(size);
}

/**
 * Write a decoded frame into `producer`, streaming from `stream` a payload its decode left
 * there. Resolves false if the group closed first, which already failed the frame.
 */
export async function receiveFrame(
	stream: Reader,
	producer: group.Producer,
	frame: WirePayload,
	inflated?: number,
): Promise<boolean> {
	if (frame.payload) {
		hooks.writeGroupFrame(producer, frame as group.Frame, inflated);
		return true;
	}
	const open = producer.createFrame({ size: frame.size, timestamp: frame.timestamp });
	while (open.remaining > 0) {
		const chunk = await race([stream.readChunk(Math.min(open.remaining, SEGMENT_BYTES)), producer.closed]);
		if (!(chunk instanceof Uint8Array)) return false;
		open.write(chunk);
	}
	open.finish();
	return true;
}

/** Read past a `size`-byte payload left on the stream, keeping none of it. */
export async function skipPayload(stream: Reader, size: number): Promise<void> {
	for (let left = size; left > 0; ) left -= (await stream.readChunk(Math.min(left, SEGMENT_BYTES))).byteLength;
}

/** The payload size of a frame read for egress. */
export function readSize(read: ReadGroupFrame): number {
	return read.chunks?.size ?? read.frame.payload.byteLength;
}

/** Write a streamed payload with `write` as it arrives. Throws if `closed` settles first, since the stream can take no more. */
export async function sendPayload(
	chunks: group.FrameConsumer,
	write: (chunk: Uint8Array) => Promise<unknown>,
	closed: Promise<unknown>,
): Promise<void> {
	for (let sent = 0; ; ) {
		const chunk = await race([chunks.readChunk(), closed]);
		if (!(chunk instanceof Uint8Array)) {
			if (sent < chunks.size) throw new Error("stream closed mid-frame");
			return;
		}
		sent += chunk.byteLength;
		await write(chunk);
	}
}

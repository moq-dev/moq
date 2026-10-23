import { concat, encodeVarint, Reader, Writer } from "../stream.ts";
import { decodeError } from "../util/decode_error.ts";

/**
 * The largest message body either side accepts, matching the Rust implementation: the
 * same ceiling as SETUP, checked at the length prefix before the body is buffered.
 */
export const MAX_MESSAGE_SIZE = 0xffff;

// Encodes a message with a varint size prefix, refusing a body the peer would reject. The whole
// message, type `id` included, goes out in one write, so a refused one leaves nothing on the stream.
export async function encode(
	writer: Writer,
	f: (w: Writer) => Promise<void>,
	{ id, max = MAX_MESSAGE_SIZE }: { id?: number; max?: number } = {},
) {
	const body = await Writer.collect(writer.version, f);
	if (body.byteLength > max) {
		throw new Error(`message too large: ${body.byteLength} bytes (max ${max})`);
	}

	const size = encodeVarint(body.byteLength, writer.version);
	await writer.write(concat(id === undefined ? [size, body] : [encodeVarint(id, writer.version), size, body]));
}

// Reads a message with a varint size prefix, refusing an oversized body before reading it.
export async function decode<T>(reader: Reader, f: (r: Reader) => Promise<T>, max = MAX_MESSAGE_SIZE): Promise<T> {
	const size = await reader.u53();
	if (size > max) {
		throw decodeError(new Error(`message too large: ${size} bytes (max ${max})`));
	}
	const data = await reader.read(size);

	// Once all declared bytes arrived, running short or leaving bytes unread is
	// malformed input. A transport FIN/reset while filling above remains distinct.
	try {
		const limit = new Reader(undefined, data, reader.version);
		const msg = await f(limit);
		if (!(await limit.done())) throw new Error("Message decoding consumed too few bytes");
		return msg;
	} catch (err: unknown) {
		throw decodeError(err);
	}
}

export async function decodeMaybe<T>(reader: Reader, f: (r: Reader) => Promise<T>): Promise<T | undefined> {
	if (await reader.done()) return;
	return await decode(reader, f);
}

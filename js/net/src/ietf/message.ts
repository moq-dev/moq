import { concat, encodeVarint, Reader, type StreamVersion, Writer } from "../stream.ts";
import { MalformedAuthToken } from "./token.ts";
import type { IetfVersion } from "./version.ts";

// Encodes a message with a u16 (16-bit) size prefix as per draft-14, after its type `id` if
// given. The whole message goes out in one write, so an oversized one leaves nothing on the stream.
export async function encode(writer: Writer, f: (w: Writer) => Promise<void>, id?: number) {
	const body = await Writer.collect(writer.version, f);
	if (body.byteLength > 65535) {
		throw new Error(`Message too large: ${body.byteLength} bytes (max 65535)`);
	}

	const size = new Uint8Array([body.byteLength >> 8, body.byteLength & 0xff]);
	const parts = id === undefined ? [size, body] : [encodeVarint(id, writer.version), size, body];
	await writer.write(concat(parts));
}

/** A message that encodes everything but its type. */
export interface Encode {
	encode(w: Writer, version: IetfVersion): Promise<void>;
}

// Writes `msg` after the type `id` the caller picked for it, in one write, so a message that
// fails to encode leaves nothing on the stream.
export async function write(writer: Writer, id: number, msg: Encode, version: IetfVersion) {
	await writer.write(await frame(writer.version, id, msg, version));
}

// The one chunk `write` sends, for a caller that must tell a failed encode from a failed write.
export async function frame(stream: StreamVersion, id: number, msg: Encode, version: IetfVersion) {
	const body = await Writer.collect(stream, (w) => msg.encode(w, version));
	return concat([encodeVarint(id, stream), body]);
}

// Reads a message with a u16 size prefix.
export async function decode<T>(reader: Reader, f: (r: Reader) => Promise<T>): Promise<T> {
	const size = await reader.u16();
	const data = await reader.read(size);

	const limit = new Reader(undefined, data, reader.version);
	let msg: T;
	try {
		msg = await f(limit);
	} catch (err) {
		// Every request starts with its Request ID, which the refusal names.
		if (err instanceof MalformedAuthToken) err.requestId = await new Reader(undefined, data, reader.version).u62();
		throw err;
	}

	// Check that we consumed exactly the right number of bytes
	if (!(await limit.done())) {
		throw new Error("Message decoding consumed too few bytes");
	}

	return msg;
}

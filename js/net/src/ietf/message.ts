import { asIetf, concat, encodeVarint, Reader, type StreamVersion, Writer } from "../stream.ts";
import { MalformedAuthToken } from "./token.ts";
import { type IetfVersion, isDraft23 } from "./version.ts";

/**
 * The largest control message body sent or accepted. Draft-23's MAX_CONTROL_MESSAGE_SIZE
 * (section 9.1.8) defaults to it and we never advertise more, so it is the bound a 16-bit
 * length already implied.
 */
export const MAX_CONTROL_MESSAGE_SIZE = 65535;

// Draft-23 (section 9) makes the Message Length a varint, SETUP's included; earlier drafts use 16 bits.
function varintLength(stream: StreamVersion): boolean {
	const version = asIetf(stream);
	return version !== undefined && isDraft23(version);
}

/** A control message's Message Length in `stream`'s encoding. */
export function lengthBytes(size: number, stream: StreamVersion): Uint8Array {
	if (varintLength(stream)) return encodeVarint(size, stream);
	return new Uint8Array([size >> 8, size & 0xff]);
}

// Reads a Message Length, refusing one past MAX_CONTROL_MESSAGE_SIZE before its body is buffered.
export async function readLength(reader: Reader): Promise<number> {
	if (!varintLength(reader.version)) return await reader.u16();
	const size = await reader.u62();
	if (size > BigInt(MAX_CONTROL_MESSAGE_SIZE)) {
		throw new Error(`Message too large: ${size} bytes (max ${MAX_CONTROL_MESSAGE_SIZE})`);
	}
	return Number(size);
}

// Encodes a message with its Message Length, after its type `id` if given. The whole message
// goes out in one write, so an oversized one leaves nothing on the stream.
export async function encode(writer: Writer, f: (w: Writer) => Promise<void>, id?: number) {
	const body = await Writer.collect(writer.version, f);
	if (body.byteLength > MAX_CONTROL_MESSAGE_SIZE) {
		throw new Error(`Message too large: ${body.byteLength} bytes (max ${MAX_CONTROL_MESSAGE_SIZE})`);
	}

	const size = lengthBytes(body.byteLength, writer.version);
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

// Reads a message with its Message Length.
export async function decode<T>(reader: Reader, f: (r: Reader) => Promise<T>): Promise<T> {
	const size = await readLength(reader);
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

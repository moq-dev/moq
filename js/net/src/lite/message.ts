import { Reader, Writer } from "../stream.ts";

/**
 * The largest message body either side accepts, matching the Rust implementation: the
 * same ceiling as SETUP, checked at the length prefix before the body is buffered.
 */
const MAX_SIZE = 0xffff;

// Encodes a message with a varint size prefix, refusing a body the peer would reject.
export async function encode(writer: Writer, f: (w: Writer) => Promise<void>, max = MAX_SIZE) {
	let scratch = new Uint8Array();

	const temp = new Writer(
		new WritableStream({
			write(chunk: Uint8Array) {
				const needed = scratch.byteLength + chunk.byteLength;
				if (needed > scratch.buffer.byteLength) {
					// Resize the buffer to the needed size.
					const capacity = Math.max(needed, scratch.buffer.byteLength * 2);
					const newBuffer = new ArrayBuffer(capacity);
					const newScratch = new Uint8Array(newBuffer, 0, needed);

					// Copy the old data into the new buffer.
					newScratch.set(scratch);

					// Copy the new chunk into the new buffer.
					newScratch.set(chunk, scratch.byteLength);

					scratch = newScratch;
				} else {
					// Copy chunk data into buffer
					scratch = new Uint8Array(scratch.buffer, 0, needed);
					scratch.set(chunk, needed - chunk.byteLength);
				}
			},
		}),
		writer.version,
	);

	await f(temp);
	temp.close();
	await temp.closed;

	if (scratch.byteLength > max) {
		throw new Error(`message too large: ${scratch.byteLength} bytes (max ${max})`);
	}

	await writer.u53(scratch.byteLength);
	if (scratch.byteLength > 0) {
		await writer.write(scratch);
	}
}

// Reads a message with a varint size prefix, refusing an oversized body before reading it.
export async function decode<T>(reader: Reader, f: (r: Reader) => Promise<T>, max = MAX_SIZE): Promise<T> {
	const size = await reader.u53();
	if (size > max) {
		throw new Error(`message too large: ${size} bytes (max ${max})`);
	}
	const data = await reader.read(size);

	const limit = new Reader(undefined, data, reader.version);
	const msg = await f(limit);

	// Check that we consumed exactly the right number of bytes
	if (!(await limit.done())) {
		throw new Error("Message decoding consumed too few bytes");
	}

	return msg;
}

export async function decodeMaybe<T>(reader: Reader, f: (r: Reader) => Promise<T>): Promise<T | undefined> {
	if (await reader.done()) return;
	return await decode(reader, f);
}

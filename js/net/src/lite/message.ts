import { Reader, Writer } from "../stream.ts";

export async function encodePayload(f: (w: Writer) => Promise<void>): Promise<Uint8Array> {
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
	);

	await f(temp);
	temp.close();
	await temp.closed;

	return scratch;
}

export async function encodeBytes(writer: Writer, scratch: Uint8Array) {
	await writer.u53(scratch.byteLength);
	if (scratch.byteLength > 0) {
		await writer.write(scratch);
	}
}

export async function encode(writer: Writer, f: (w: Writer) => Promise<void>) {
	return encodeBytes(writer, await encodePayload(f));
}

export async function decodeBytes(reader: Reader): Promise<Uint8Array> {
	const size = await reader.u53();
	return reader.read(size);
}

export async function decodePayload<T>(payload: Uint8Array, f: (r: Reader) => Promise<T>): Promise<T> {
	const limit = new Reader(undefined, payload);
	const msg = await f(limit);

	// Check that we consumed exactly the right number of bytes
	if (!(await limit.done())) {
		throw new Error("Message decoding consumed too few bytes");
	}

	return msg;
}

// Reads a message with a varint size prefix.
export async function decode<T>(reader: Reader, f: (r: Reader) => Promise<T>): Promise<T> {
	return decodePayload(await decodeBytes(reader), f);
}

export async function decodeMaybe<T>(reader: Reader, f: (r: Reader) => Promise<T>): Promise<T | undefined> {
	if (await reader.done()) return;
	return await decode(reader, f);
}

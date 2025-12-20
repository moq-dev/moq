import { Reader, Writer } from "../stream.ts";

export async function encode(writer: Writer, f: (w: Writer) => Promise<void>) {
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

	await writer.u53(scratch.byteLength);
	await writer.write(scratch);
}

// Reads a message with a varint size prefix.
export async function decode<T>(reader: Reader, f: (r: Reader) => Promise<T>): Promise<T> {
	const size = await reader.u53();
	const data = await reader.read(size);

	const limit = new Reader(undefined, data);
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

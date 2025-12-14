import { Reader, Writer } from "../stream.ts";

// Encodes a message with a u16 (16-bit) size prefix as per draft-14.
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

	try {
		await f(temp);
	} finally {
		temp.close();
	}

	await temp.closed;

	// Check that message fits in u16
	if (scratch.byteLength > 65535) {
		throw new Error(`Message too large: ${scratch.byteLength} bytes (max 65535)`);
	}

	// Write u16 size (2 bytes, big-endian)
	await writer.u16(scratch.byteLength);
	await writer.write(scratch);
}

// Reads a message with a u16 size prefix.
export async function decode<T>(reader: Reader, f: (r: Reader) => Promise<T>): Promise<T> {
	const size = await reader.u16();
	const data = await reader.read(size);

	const limit = new Reader(undefined, data);
	const msg = await f(limit);

	// Check that we consumed exactly the right number of bytes
	if (!(await limit.done())) {
		throw new Error("Message decoding consumed too few bytes");
	}

	return msg;
}

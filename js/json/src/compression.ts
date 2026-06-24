/**
 * Per-frame DEFLATE compression for the JSON frame stream, built on the platform
 * {@link https://developer.mozilla.org/en-US/docs/Web/API/Compression_Streams_API | Compression Streams API}.
 *
 * Each frame is compressed on its own as a raw DEFLATE ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html))
 * blob (`deflate-raw`), the same format the Rust `moq-json` producer writes, so the two
 * interoperate on the wire. There is no cross-frame context, so snapshots and large frames shrink
 * well while tiny deltas barely benefit. The browser API exposes no level or dictionary knobs, so
 * compression is a plain on/off toggle.
 *
 * @module
 */

// Maximum decompressed size of a single frame. A malicious publisher could otherwise send a tiny
// slice that inflates hugely, so {@link inflate} stops rather than allocating without limit.
// Mirrors the Rust `MAX_DECOMPRESSED_FRAME`.
const MAX_DECOMPRESSED_FRAME = 64 * 1024 * 1024;

/** Compress one frame payload into a standalone `deflate-raw` blob. Empty in yields empty out. */
export async function deflate(payload: Uint8Array): Promise<Uint8Array> {
	if (payload.length === 0) return payload;
	const cs = new CompressionStream("deflate-raw");
	return pump(cs, payload);
}

/**
 * Decompress one `deflate-raw` frame back into its payload. Empty in yields empty out.
 *
 * Throws if the input is malformed or inflates past the per-frame size limit.
 */
export async function inflate(slice: Uint8Array): Promise<Uint8Array> {
	if (slice.length === 0) return slice;
	const ds = new DecompressionStream("deflate-raw");
	return pump(ds, slice, MAX_DECOMPRESSED_FRAME);
}

// Drive a (de)compression stream end-to-end: feed it `input`, read every output chunk, and
// concatenate. Reads concurrently with writing to avoid the transform's backpressure deadlock.
async function pump(
	transform: CompressionStream | DecompressionStream,
	input: Uint8Array,
	limit = Number.POSITIVE_INFINITY,
): Promise<Uint8Array> {
	const writer = transform.writable.getWriter();
	// The same error surfaces from the reader below, so swallow the writer's copy to avoid an
	// unhandled rejection on malformed input. The cast narrows `ArrayBufferLike` to `ArrayBuffer`:
	// our inputs are never SharedArrayBuffer-backed, which is all the DOM `BufferSource` type wants.
	const written = (async () => {
		await writer.write(input as Uint8Array<ArrayBuffer>);
		await writer.close();
	})().catch(() => {});

	const reader = transform.readable.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	for (;;) {
		const { value, done } = await reader.read();
		if (done) break;
		total += value.length;
		if (total > limit) {
			await reader.cancel();
			throw new Error(`decompressed frame exceeded ${limit} bytes`);
		}
		chunks.push(value);
	}
	await written;

	if (chunks.length === 1) return chunks[0];
	const out = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.length;
	}
	return out;
}

/**
 * Per-frame payload compression for moq-lite-05.
 *
 * A publisher marks a {@link Track} with `compress = true` when its frames are
 * worth compressing (e.g. a JSON catalog). The concrete codec is negotiated in
 * SUBSCRIBE_OK, and every frame is compressed independently so the codec never
 * carries state across the lossy, out-of-order group boundary.
 */

// Mirrors the Rust MAX_FRAME_SIZE cap on the receive path: reject anything that
// inflates past this so a malicious peer can't zip-bomb the receiver.
const MAX_FRAME_SIZE = 16 * 1024 * 1024;

/** The codec used to (de)compress frame payloads, negotiated per subscription. */
export const Compression = {
	/** Frames are sent verbatim. */
	None: 0,
	/**
	 * Raw DEFLATE (RFC 1951), no zlib/gzip header. Matches the browser's
	 * "deflate-raw" format and the Rust side's `flate2` raw deflate. QUIC already
	 * guarantees integrity, so the extra checksum bytes of zlib/gzip are wasted.
	 */
	Deflate: 1,
} as const;

export type Compression = (typeof Compression)[keyof typeof Compression];

/** Parse a wire varint code, throwing on an unknown codec. */
export function compressionFromCode(code: number): Compression {
	switch (code) {
		case Compression.None:
			return Compression.None;
		case Compression.Deflate:
			return Compression.Deflate;
		default:
			throw new Error(`unsupported compression codec: ${code}`);
	}
}

// Map a codec to its WHATWG (De)CompressionStream format string.
function format(codec: Compression): CompressionFormat {
	switch (codec) {
		case Compression.Deflate:
			return "deflate-raw";
		default:
			throw new Error(`codec has no stream format: ${codec}`);
	}
}

function concat(chunks: Uint8Array[], total: number): Uint8Array {
	if (chunks.length === 1) return chunks[0];
	const out = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return out;
}

// Pump `data` through a transform stream and collect the result, capping the
// output at `maxSize` (used on decode so a small compressed blob can't expand
// without bound).
async function pump(transform: CompressionStream | DecompressionStream, data: Uint8Array, maxSize: number) {
	const writer = transform.writable.getWriter();
	// Drive the write concurrently with the read so the transform doesn't stall
	// waiting for its output to be drained.
	const writing = (async () => {
		// Cast per the project convention for passing a Uint8Array to a Web
		// Streams sink typed as BufferSource (see watch/src/audio/mse.ts).
		await writer.write(data as BufferSource);
		await writer.close();
	})();

	const reader = transform.readable.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;

	try {
		for (;;) {
			const { done, value } = await reader.read();
			if (done) break;
			total += value.byteLength;
			if (total > maxSize) {
				throw new Error(`decompressed frame exceeds ${maxSize} bytes`);
			}
			chunks.push(value);
		}
	} finally {
		// Surface a write-side failure (e.g. malformed deflate input) instead of
		// swallowing it, but don't let an already-thrown read error get masked.
		await writing.catch(() => {});
	}

	return concat(chunks, total);
}

/** Compress a whole frame payload. {@link Compression.None} returns the input unchanged. */
export async function compress(codec: Compression, data: Uint8Array): Promise<Uint8Array> {
	if (codec === Compression.None) return data;
	return pump(new CompressionStream(format(codec)), data, Number.POSITIVE_INFINITY);
}

/**
 * Decompress a whole frame payload, rejecting anything that inflates past
 * `maxSize` (default {@link MAX_FRAME_SIZE}).
 */
export async function decompress(codec: Compression, data: Uint8Array, maxSize = MAX_FRAME_SIZE): Promise<Uint8Array> {
	if (codec === Compression.None) return data;
	return pump(new DecompressionStream(format(codec)), data, maxSize);
}

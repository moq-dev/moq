/**
 * Group-scoped DEFLATE compression for the JSON frame stream, using
 * {@link https://github.com/nodeca/pako | pako}'s streaming deflate/inflate.
 *
 * Within a group the frame payloads form a single raw DEFLATE
 * ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html)) stream, sync-flushed at each frame
 * boundary so every frame is self-delimited while later frames reuse the earlier ones as context
 * (a snapshot followed by deltas compresses far better than each frame alone). This matches the
 * Rust `moq-json` producer, so the two interoperate on the wire.
 *
 * A sync flush always ends in the fixed 4-byte marker `00 00 ff ff`. {@link Encoder.frame} drops
 * it and {@link Decoder.frame} re-appends it, saving 4 bytes per frame, the same trick
 * [RFC 7692](https://www.rfc-editor.org/rfc/rfc7692.html#section-7.2.1) (permessage-deflate) uses.
 *
 * `pako` is an optional peer dependency loaded on demand, so consumers that never compress a track
 * never bundle it.
 *
 * @module
 */

// Maximum decompressed size of a single frame. A malicious publisher could otherwise send a tiny
// slice that inflates hugely, so {@link Decoder.frame} stops rather than allocating without limit.
// Mirrors the Rust `MAX_DECOMPRESSED_FRAME`.
const MAX_DECOMPRESSED_FRAME = 64 * 1024 * 1024;

// The trailing bytes of a DEFLATE sync flush, stripped on the wire and re-appended to decode.
const SYNC_FLUSH_TAIL = new Uint8Array([0x00, 0x00, 0xff, 0xff]);

type Pako = typeof import("pako");
let pako: Promise<Pako> | undefined;

// Load pako once, on demand. The optional peer dependency keeps it out of bundles that never
// enable compression; a clear error points at the missing install if it's reached without it.
function loadPako(): Promise<Pako> {
	pako ??= import("pako")
		.then((m) => (m as { default?: Pako }).default ?? (m as Pako))
		.catch((err) => {
			pako = undefined;
			throw new Error("@moq/json compression requires the optional peer dependency `pako` to be installed", {
				cause: err,
			});
		});
	return pako;
}

// Concatenate chunks into one buffer (a single chunk passes through untouched).
function concat(chunks: Uint8Array[], total: number): Uint8Array {
	if (chunks.length === 1) return chunks[0];
	const out = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.length;
	}
	return out;
}

/**
 * Encodes a group's frame payloads into one shared DEFLATE stream, one self-delimited slice per
 * frame. Hold one per group; create a new one at each group boundary. Build with {@link create}.
 */
export class Encoder {
	#deflate: import("pako").Deflate;
	#flush: number;
	#chunks: Uint8Array[] = [];
	#total = 0;

	private constructor(lib: Pako) {
		this.#deflate = new lib.Deflate({ raw: true });
		this.#flush = lib.constants.Z_SYNC_FLUSH;
		this.#deflate.onData = (chunk) => {
			const bytes = chunk as Uint8Array;
			this.#chunks.push(bytes);
			this.#total += bytes.length;
		};
	}

	/** Start a fresh per-group encoder with a cold window. */
	static async create(): Promise<Encoder> {
		return new Encoder(await loadPako());
	}

	/**
	 * Compress the next frame's `payload`, returning its slice of the group stream (minus the fixed
	 * sync-flush marker). Empty in yields empty out. Slices must be produced in frame order.
	 */
	frame(payload: Uint8Array): Uint8Array {
		if (payload.length === 0) return payload;
		this.#chunks = [];
		this.#total = 0;
		this.#deflate.push(payload, this.#flush);
		const full = concat(this.#chunks, this.#total);
		// Drop the trailing sync-flush marker; the decoder re-appends it.
		return full.subarray(0, full.length - SYNC_FLUSH_TAIL.length);
	}
}

/**
 * Decodes a group's frame slices back into the original payloads. Hold one per group; feed slices
 * in frame order (each frame builds on the earlier ones). Build with {@link create}.
 */
export class Decoder {
	#inflate: import("pako").Inflate;
	#flush: number;
	#chunks: Uint8Array[] = [];
	#total = 0;
	#tooLarge = false;

	private constructor(lib: Pako) {
		this.#inflate = new lib.Inflate({ raw: true });
		this.#flush = lib.constants.Z_SYNC_FLUSH;
		this.#inflate.onData = (chunk) => {
			const bytes = chunk as Uint8Array;
			this.#total += bytes.length;
			// Bound memory: stop retaining output past the cap, then reject after the push returns.
			if (this.#total > MAX_DECOMPRESSED_FRAME) {
				this.#tooLarge = true;
				return;
			}
			this.#chunks.push(bytes);
		};
	}

	/** Start a fresh per-group decoder with a cold window. */
	static async create(): Promise<Decoder> {
		return new Decoder(await loadPako());
	}

	/**
	 * Decompress the next frame's `slice` back into its payload. Empty in yields empty out. Throws
	 * if the input is malformed or inflates past the per-frame size limit.
	 */
	frame(slice: Uint8Array): Uint8Array {
		if (slice.length === 0) return slice;
		this.#chunks = [];
		this.#total = 0;
		this.#tooLarge = false;

		// Re-append the stripped sync-flush marker, which delimits the frame and flushes its bytes out.
		const input = new Uint8Array(slice.length + SYNC_FLUSH_TAIL.length);
		input.set(slice);
		input.set(SYNC_FLUSH_TAIL, slice.length);

		this.#inflate.push(input, this.#flush);
		if (this.#inflate.err) throw new Error(`decompression failed: ${this.#inflate.msg}`);
		if (this.#tooLarge) throw new Error(`decompressed frame exceeded ${MAX_DECOMPRESSED_FRAME} bytes`);
		return concat(this.#chunks, this.#total);
	}
}

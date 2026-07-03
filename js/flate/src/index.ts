/**
 * Group-scoped DEFLATE: a stream of self-delimited frames sharing one compression window, using
 * {@link https://github.com/nodeca/pako | pako}'s streaming deflate/inflate.
 *
 * A sequence of frame payloads is compressed into a single raw DEFLATE
 * ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html)) stream, sync-flushed at each frame
 * boundary, so every frame is self-delimited (byte-aligned, the window retained) while later frames
 * reuse the earlier ones as context. A stream of similar payloads (a snapshot followed by deltas,
 * repeated records, log lines) compresses far better than each payload alone. Create a fresh
 * {@link Encoder}/{@link Decoder} pair per independent stream (in moq-net terms, per group).
 *
 * This is plain raw DEFLATE with a `Z_SYNC_FLUSH` after each frame, so any peer using the same
 * primitive (the Rust `moq-flate` crate, zlib's sync flush) interoperates on the wire. There is no
 * length prefix: the caller frames each slice (moq-net already does).
 *
 * A sync flush always ends in the fixed 4-byte marker `00 00 ff ff`. {@link Encoder.frame} drops it
 * and {@link Decoder.frame} re-appends it, saving 4 bytes per frame, the same trick
 * [RFC 7692](https://www.rfc-editor.org/rfc/rfc7692.html#section-7.2.1) (permessage-deflate) uses. A
 * small slice can still inflate enormously, so {@link Decoder.frame} caps the inflated output as it
 * is produced.
 *
 * pako is synchronous, so the whole codec is synchronous; it is a normal dependency.
 *
 * @module
 */

import * as pako from "pako";

/** The default DEFLATE level ({@link Encoder}): a good size/speed balance for small, repetitive payloads. */
export const DEFAULT_LEVEL = 6;

/** The default per-frame decompressed-size cap ({@link Decoder}): 64 MiB. */
export const DEFAULT_MAX_FRAME_SIZE = 64 * 1024 * 1024;

// The trailing bytes of a DEFLATE sync flush, stripped on the wire and re-appended to decode.
const SYNC_FLUSH_TAIL = new Uint8Array([0x00, 0x00, 0xff, 0xff]);

// Concatenate chunks into one tight buffer (a single chunk passes through untouched). Safe only for
// output that is consumed before the next push, since a single chunk is pako's own reused buffer.
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

/** Options for an {@link Encoder}. */
export interface EncoderOptions {
	/** DEFLATE level, `0..=9` (higher is smaller and slower). Defaults to {@link DEFAULT_LEVEL}. */
	level?: number;
}

/**
 * Encodes a stream's frame payloads into one shared DEFLATE window, one self-delimited slice per
 * frame. Hold one per stream; create a new one for each independent stream.
 *
 * @public
 */
export class Encoder {
	#deflate: pako.Deflate;
	#chunks: Uint8Array[] = [];
	#total = 0;

	/** Start a fresh encoder with a cold window. */
	constructor(options: EncoderOptions = {}) {
		// `raw`: no zlib header/trailer, matching the Rust side and the browser's `deflate-raw`.
		// pako types `level` as a literal union; we accept a plain number and narrow here.
		const level = (options.level ?? DEFAULT_LEVEL) as pako.DeflateOptions["level"];
		this.#deflate = new pako.Deflate({ raw: true, level });
		this.#deflate.onData = (chunk) => {
			const bytes = chunk as Uint8Array;
			this.#chunks.push(bytes);
			this.#total += bytes.length;
		};
	}

	/**
	 * Compress the next frame's `payload`, returning its slice of the stream: the DEFLATE bytes minus
	 * the fixed sync-flush marker. Empty in yields empty out. Slices must be produced in frame order.
	 */
	frame(payload: Uint8Array): Uint8Array {
		if (payload.length === 0) return payload;
		this.#chunks = [];
		this.#total = 0;
		this.#deflate.push(payload, pako.constants.Z_SYNC_FLUSH);

		// Copy into one tight owned buffer, dropping the trailing sync-flush marker. We can't return
		// pako's chunk views: a caller retains the reference and pako backs each chunk with a ~16 KB
		// buffer, so a view would pin far more memory than the frame.
		const out = new Uint8Array(this.#total - SYNC_FLUSH_TAIL.length);
		let offset = 0;
		for (const chunk of this.#chunks) {
			if (offset >= out.length) break;
			const take = Math.min(chunk.length, out.length - offset);
			out.set(chunk.subarray(0, take), offset);
			offset += take;
		}
		return out;
	}
}

/** Options for a {@link Decoder}. */
export interface DecoderOptions {
	/**
	 * Per-frame decompressed-size cap. A frame that inflates past this is rejected (zip-bomb guard).
	 * Defaults to {@link DEFAULT_MAX_FRAME_SIZE}.
	 */
	maxFrameSize?: number;
}

/**
 * Decodes a stream's frame slices back into the original payloads. Hold one per stream; feed slices
 * in frame order (each frame builds on the earlier ones).
 *
 * @public
 */
export class Decoder {
	#inflate = new pako.Inflate({ raw: true });
	#chunks: Uint8Array[] = [];
	#total = 0;
	#tooLarge = false;
	#maxFrameSize: number;

	/** Start a fresh decoder with a cold window. */
	constructor(options: DecoderOptions = {}) {
		this.#maxFrameSize = options.maxFrameSize ?? DEFAULT_MAX_FRAME_SIZE;
		this.#inflate.onData = (chunk) => {
			const bytes = chunk as Uint8Array;
			this.#total += bytes.length;
			// Bound the inflated output as it is produced; a tiny slice can expand enormously. Stop
			// retaining past the cap, then reject once the push returns.
			if (this.#total > this.#maxFrameSize) {
				this.#tooLarge = true;
				return;
			}
			this.#chunks.push(bytes);
		};
	}

	/**
	 * Decompress the next frame's `slice` back into its payload. Empty in yields empty out. Throws if
	 * the input is malformed or inflates past the per-frame size cap.
	 */
	frame(slice: Uint8Array): Uint8Array {
		if (slice.length === 0) return slice;

		this.#chunks = [];
		this.#total = 0;
		this.#tooLarge = false;

		// Feed the slice then the re-appended sync-flush marker as two pushes, so no combined buffer is
		// allocated. The marker delimits the frame and flushes its last bytes out of the inflate buffer.
		this.#inflate.push(slice, false);
		this.#inflate.push(SYNC_FLUSH_TAIL, pako.constants.Z_SYNC_FLUSH);
		if (this.#inflate.err) throw new Error(`decompression failed: ${this.#inflate.msg}`);
		if (this.#tooLarge) throw new Error(`decompressed frame exceeded ${this.#maxFrameSize} bytes`);

		return concat(this.#chunks, this.#total);
	}
}

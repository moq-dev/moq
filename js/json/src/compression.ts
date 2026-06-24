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
 * moq-net frames each slice, so there's no length prefix; {@link Decoder.frame} instead caps the
 * inflated output as it is produced.
 *
 * pako is synchronous, so the whole codec is synchronous; it is a normal dependency.
 *
 * @module
 */

import * as pako from "pako";

// Maximum decompressed size of a single frame. A malicious publisher could otherwise send a tiny
// slice that inflates hugely, so {@link Decoder} stops retaining output past this and rejects the
// frame. Mirrors the Rust `MAX_DECOMPRESSED_FRAME`.
const MAX_DECOMPRESSED_FRAME = 64 * 1024 * 1024;

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

/**
 * Encodes a group's frame payloads into one shared DEFLATE stream, one self-delimited slice per
 * frame. Hold one per group; create a new one at each group boundary.
 *
 * @public
 */
export class Encoder {
	#deflate = new pako.Deflate({ raw: true });
	#chunks: Uint8Array[] = [];
	#total = 0;

	/** Start a fresh per-group encoder with a cold window. */
	constructor() {
		this.#deflate.onData = (chunk) => {
			const bytes = chunk as Uint8Array;
			this.#chunks.push(bytes);
			this.#total += bytes.length;
		};
	}

	/**
	 * Compress the next frame's `payload`, returning its slice of the group stream: the DEFLATE bytes
	 * minus the fixed sync-flush marker. Empty in yields empty out. Slices must be produced in frame
	 * order.
	 */
	frame(payload: Uint8Array): Uint8Array {
		if (payload.length === 0) return payload;
		this.#chunks = [];
		this.#total = 0;
		this.#deflate.push(payload, pako.constants.Z_SYNC_FLUSH);

		// Copy into one tight owned buffer, dropping the trailing sync-flush marker. We can't return
		// pako's chunk views: `writeFrame` retains the reference and pako backs each chunk with a
		// ~16 KB buffer, so a view would pin far more memory than the frame.
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

/**
 * Decodes a group's frame slices back into the original payloads. Hold one per group; feed slices
 * in frame order (each frame builds on the earlier ones).
 *
 * @public
 */
export class Decoder {
	#inflate = new pako.Inflate({ raw: true });
	#chunks: Uint8Array[] = [];
	#total = 0;
	#tooLarge = false;

	/** Start a fresh per-group decoder with a cold window. */
	constructor() {
		this.#inflate.onData = (chunk) => {
			const bytes = chunk as Uint8Array;
			this.#total += bytes.length;
			// Bound the inflated output as it is produced; a tiny slice can expand enormously. Stop
			// retaining past the cap, then reject once the push returns.
			if (this.#total > MAX_DECOMPRESSED_FRAME) {
				this.#tooLarge = true;
				return;
			}
			this.#chunks.push(bytes);
		};
	}

	/**
	 * Decompress the next frame's `slice` back into its payload. Empty in yields empty out. Throws if
	 * the input is malformed or inflates past the per-frame size limit.
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
		if (this.#tooLarge) throw new Error(`decompressed frame exceeded ${MAX_DECOMPRESSED_FRAME} bytes`);

		return concat(this.#chunks, this.#total);
	}
}

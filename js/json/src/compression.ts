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
 * Each slice is prefixed with its decompressed length as a QUIC varint (via `@moq/net`'s `Varint`),
 * so the decoder bounds the frame before inflating and matches the Rust producer on the wire.
 *
 * pako is synchronous, so the whole codec is synchronous: only an enabled track pulls it in, but it
 * is a normal dependency rather than a lazily loaded one.
 *
 * @module
 */

import { Varint } from "@moq/net";
import * as pako from "pako";

// Maximum decompressed size of a single frame. A malicious publisher could otherwise send a tiny
// slice that inflates hugely, so {@link Decoder.frame} stops rather than allocating without limit.
// Mirrors the Rust `MAX_DECOMPRESSED_FRAME`.
const MAX_DECOMPRESSED_FRAME = 64 * 1024 * 1024;

// The trailing bytes of a DEFLATE sync flush, stripped on the wire and re-appended to decode.
const SYNC_FLUSH_TAIL = new Uint8Array([0x00, 0x00, 0xff, 0xff]);

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
	 * Compress the next frame's `payload`, returning its slice of the group stream: a decompressed-
	 * length varint prefix, then the DEFLATE bytes minus the fixed sync-flush marker. Empty in yields
	 * empty out. Slices must be produced in frame order.
	 */
	frame(payload: Uint8Array): Uint8Array {
		if (payload.length === 0) return payload;
		this.#chunks = [];
		this.#total = 0;
		this.#deflate.push(payload, pako.constants.Z_SYNC_FLUSH);
		const full = concat(this.#chunks, this.#total);
		// Drop the trailing sync-flush marker (the decoder re-appends it) and prefix the length.
		const deflate = full.subarray(0, full.length - SYNC_FLUSH_TAIL.length);
		const prefix = Varint.encode(payload.length);
		const out = new Uint8Array(prefix.length + deflate.length);
		out.set(prefix);
		out.set(deflate, prefix.length);
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
			// Bound memory: stop retaining output past the cap, then reject after the push returns.
			if (this.#total > MAX_DECOMPRESSED_FRAME) {
				this.#tooLarge = true;
				return;
			}
			this.#chunks.push(bytes);
		};
	}

	/**
	 * Decompress the next frame's `slice` back into its payload. Empty in yields empty out. Throws
	 * if the input is malformed, declares more than the per-frame size limit, or inflates to a length
	 * that doesn't match its prefix.
	 */
	frame(slice: Uint8Array): Uint8Array {
		if (slice.length === 0) return slice;

		// The decompressed-length prefix bounds the frame before any inflation.
		const [declared, deflate] = Varint.decode(slice);
		if (declared > MAX_DECOMPRESSED_FRAME) {
			throw new Error(`decompressed frame exceeded ${MAX_DECOMPRESSED_FRAME} bytes`);
		}

		this.#chunks = [];
		this.#total = 0;
		this.#tooLarge = false;

		// Re-append the stripped sync-flush marker, which delimits the frame and flushes its bytes out.
		const input = new Uint8Array(deflate.length + SYNC_FLUSH_TAIL.length);
		input.set(deflate);
		input.set(SYNC_FLUSH_TAIL, deflate.length);

		this.#inflate.push(input, pako.constants.Z_SYNC_FLUSH);
		if (this.#inflate.err) throw new Error(`decompression failed: ${this.#inflate.msg}`);
		if (this.#tooLarge) throw new Error(`decompressed frame exceeded ${MAX_DECOMPRESSED_FRAME} bytes`);

		const out = concat(this.#chunks, this.#total);
		// A mismatch with the declared length means a corrupt or lying frame.
		if (out.length !== declared) {
			throw new Error(`decompressed length mismatch: expected ${declared}, got ${out.length}`);
		}
		return out;
	}
}

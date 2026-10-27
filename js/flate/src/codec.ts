// The group-scoped DEFLATE codec: see the module docs in index.ts.

import { Group } from "@moq/net";
import * as pako from "pako";

/** The default DEFLATE level ({@link Encoder}): a good size/speed balance for small, repetitive payloads. */
export const DEFAULT_LEVEL = 6;

// The trailing bytes of a DEFLATE sync flush, stripped on the wire and re-appended to decode.
const SYNC_FLUSH_TAIL = new Uint8Array([0x00, 0x00, 0xff, 0xff]);

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
	 * The most bytes {@link frame} can return for a payload of `len` bytes, at any level and whatever
	 * the window already holds.
	 *
	 * zlib's `deflateBound` for its default window and memory level, which both pako and the Rust
	 * `moq-flate` use: incompressible input falls back to stored blocks, 5 bytes per 16 KiB. The
	 * constant covers the block headers and the flush, whose fixed 4-byte marker is stripped anyway.
	 */
	static bound(len: number): number {
		// Division rather than shifts, which wrap past 2^31.
		return len + Math.floor(len / 4096) + Math.floor(len / 16384) + Math.floor(len / 33554432) + 13;
	}

	/**
	 * Compress the next frame's `payload`, returning its slice of the stream: the DEFLATE bytes minus
	 * the fixed sync-flush marker. Empty in yields empty out. Slices must be produced in frame order.
	 */
	frame(payload: Uint8Array): Uint8Array {
		if (payload.length === 0) return payload;
		this.#chunks = [];
		this.#total = 0;
		this.#deflate.push(payload, pako.Z_SYNC_FLUSH);

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

/**
 * Decodes a stream's frame slices back into the original payloads. Hold one per stream; feed slices
 * in frame order (each frame builds on the earlier ones).
 *
 * A peer could send tiny slices that inflate hugely, so the window's total output is held to the
 * group budget, `Group.MAX_GROUP_CACHE_BYTES`: the most a plain group can carry. A compressed group
 * then costs a reader no more than a plain one, however small its slices.
 *
 * @public
 */
export class Decoder {
	#inflate = new pako.Inflate({ raw: true });
	#chunks: Uint8Array[] = [];
	#total = 0;
	// Bytes the whole window has inflated to, across every frame.
	#inflated = 0;
	#tooLarge = false;

	/** Start a fresh decoder with a cold window. */
	constructor() {
		this.#inflate.onData = (chunk) => {
			const bytes = chunk as Uint8Array;
			this.#total += bytes.length;
			this.#inflated += bytes.length;
			// Bound the inflated output as it is produced; a tiny slice can expand enormously. Stop
			// inside push rather than inflating the rest of a hostile slice.
			if (this.#inflated > Group.MAX_GROUP_CACHE_BYTES) {
				this.#tooLarge = true;
				throw new Error(`decompressed group exceeded ${Group.MAX_GROUP_CACHE_BYTES} bytes`);
			}
			this.#chunks.push(bytes);
		};
	}

	/**
	 * Decompress the next frame's `slice` back into its payload. Empty in yields empty out. Throws if
	 * the input is malformed, or once the window's output across every frame so far passes the group
	 * budget.
	 */
	frame(slice: Uint8Array): Uint8Array {
		if (slice.length === 0) return slice;
		if (this.#tooLarge) throw new Error(`decompressed group exceeded ${Group.MAX_GROUP_CACHE_BYTES} bytes`);

		this.#chunks = [];
		this.#total = 0;

		// Feed the slice then the re-appended sync-flush marker as two pushes, so no combined buffer is
		// allocated. The marker delimits the frame and flushes its last bytes out of the inflate buffer.
		try {
			this.#inflate.push(slice, false);
			this.#inflate.push(SYNC_FLUSH_TAIL, pako.Z_SYNC_FLUSH);
			if (this.#inflate.err) throw new Error(`decompression failed: ${this.#inflate.msg}`);

			// Retained frames must own tight buffers rather than pinning pako's scratch storage.
			const out = new Uint8Array(this.#total);
			let offset = 0;
			for (const chunk of this.#chunks) {
				out.set(chunk, offset);
				offset += chunk.length;
			}
			return out;
		} finally {
			this.#chunks = [];
		}
	}
}

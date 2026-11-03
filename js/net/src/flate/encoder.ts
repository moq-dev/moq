// Synchronous model writes include the encoder in the static graph.
import { Deflate, type DeflateOptions, Z_SYNC_FLUSH } from "pako";

const DEFAULT_LEVEL = 6;
const SYNC_FLUSH_TAIL = new Uint8Array([0x00, 0x00, 0xff, 0xff]);

/** Options for an {@link Encoder}. */
interface EncoderOptions {
	/** DEFLATE level, `0..=9` (higher is smaller and slower). Defaults to {@link DEFAULT_LEVEL}. */
	level?: number;
}

/**
 * Encodes a stream's frame payloads into one shared DEFLATE window, one self-delimited slice per
 * frame. Hold one per stream; create a new one for each independent stream.
 *
 * @internal
 */
export class Encoder {
	#deflate: Deflate;
	#chunks: Uint8Array[] = [];
	#total = 0;

	/** Start a fresh encoder with a cold window. */
	constructor(options: EncoderOptions = {}) {
		// `raw`: no zlib header/trailer, matching the Rust side and the browser's `deflate-raw`.
		// pako types `level` as a literal union; we accept a plain number and narrow here.
		const level = (options.level ?? DEFAULT_LEVEL) as DeflateOptions["level"];
		this.#deflate = new Deflate({ raw: true, level });
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
		this.#deflate.push(payload, Z_SYNC_FLUSH);

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

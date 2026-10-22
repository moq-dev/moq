// The group-scoped moq-flate decoder; applications always read inflated frames.
import { Inflate, Z_SYNC_FLUSH } from "pako";
import { StreamCode, StreamError } from "../error.ts";

const SYNC_FLUSH_TAIL = new Uint8Array([0x00, 0x00, 0xff, 0xff]);

function malformed(message: string): Error {
	return new StreamError(StreamCode.MalformedTrack, { message });
}

/** Decode one group's slices in frame order, enforcing each declared size. @internal */
export class Decoder {
	#inflate = new Inflate({ raw: true, chunkSize: 4096 });
	#chunks: Uint8Array[] = [];
	#total = 0;
	#size = 0;
	#failed = false;

	constructor() {
		this.#inflate.onData = (chunk) => {
			this.#total += chunk.length;
			// Throw inside push, so a bomb stops doing work at its declaration plus one
			// scratch chunk rather than merely discarding the rest of its output.
			if (this.#total > this.#size) throw malformed("inflated frame exceeds its declared size");
			this.#chunks.push(chunk);
		};
	}

	/** Inflate a slice to exactly `size` bytes, without preallocating from that declaration. */
	frame(slice: Uint8Array, size: number): Uint8Array {
		if (this.#failed) throw malformed("decoder already failed");
		if (!Number.isSafeInteger(size) || size < 0) throw malformed("invalid inflated frame size");
		this.#chunks = [];
		this.#total = 0;
		this.#size = size;
		try {
			if (slice.length > 0) {
				this.#inflate.push(slice, false);
				if (this.#inflate.ended) throw malformed("compressed group ended its DEFLATE stream");
				this.#inflate.push(SYNC_FLUSH_TAIL, Z_SYNC_FLUSH);
				if (this.#inflate.err || this.#inflate.ended) throw malformed("malformed DEFLATE slice");
			}
			if (this.#total !== size) throw malformed("inflated frame differs from its declared size");
			// Retained frames must own tight buffers rather than pinning scratch storage.
			const out = new Uint8Array(this.#total);
			let offset = 0;
			for (const chunk of this.#chunks) {
				out.set(chunk, offset);
				offset += chunk.length;
			}
			return out;
		} catch (error) {
			this.#failed = true;
			throw error;
		} finally {
			this.#chunks = [];
		}
	}
}

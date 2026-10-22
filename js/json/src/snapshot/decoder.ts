import { DEFAULT_MAX_FRAME_SIZE, Decoder as Flate } from "@moq/flate";

import { isDeflate } from "../compression.ts";
import { merge } from "../diff.ts";
import { MissingSnapshot } from "../error.ts";
import type { Config } from "./encoder.ts";

/**
 * Reconstructs a JSON value from the snapshot and delta frames of a group.
 *
 * The track-free core of {@link Consumer}, and the mirror of {@link Encoder}. The caller reads
 * frames from wherever it likes and routes each one by its position in the group: the first frame of
 * every group is a {@link snapshot}, the rest are {@link delta}s.
 *
 * Applying and materializing are separate on purpose. Frames must be applied in order (the merge
 * patches and the DEFLATE window are both sequential), but a consumer catching up on a backlog only
 * wants the value at the head, so it applies every frame and calls {@link decode} once. A caller
 * that wants a value per frame just calls it every time.
 */
export class Decoder<T> {
	#schema?: Config<T>["schema"];
	// Whether frames are `deflate-raw` compressed. Must match the encoder's {@link Config.compression}.
	#decompress: boolean;

	// The current group's DEFLATE window, rebuilt at each snapshot.
	#flate?: Flate;
	// The reconstructed value, `undefined` until the first snapshot.
	#current?: unknown;
	// Bounds the reconstructed value's size in JSON bytes: the snapshot plus every patch applied
	// since, as a merge patch adds no more to a value than its own bytes.
	#size = 0;

	constructor(config: Pick<Config<T>, "schema" | "compression"> = {}) {
		this.#schema = config.schema;
		this.#decompress = isDeflate(config.compression);
	}

	/**
	 * Apply a group's first frame: a full snapshot that replaces the current value.
	 *
	 * Also starts the group's DEFLATE window, so this must be called at every group boundary, not
	 * only the first.
	 */
	snapshot(payload: Uint8Array): void {
		// Each group is its own compressed stream, so the window starts cold here.
		this.#flate = this.#decompress ? new Flate() : undefined;
		const plain = this.#inflate(payload);
		this.#current = this.#parse(plain);
		this.#size = plain.byteLength;
	}

	/**
	 * Apply one of a group's later frames: an RFC 7396 merge patch against the current value.
	 *
	 * Throws when no snapshot has been applied yet, since a patch has nothing to apply to, or when
	 * the result would outgrow the largest snapshot a producer publishes. A refused patch drops the
	 * value until the next snapshot.
	 */
	delta(payload: Uint8Array): void {
		if (this.#current === undefined) throw new MissingSnapshot();
		const plain = this.#inflate(payload);
		this.#current = merge(this.#current, this.#parse(plain));
		this.#size += plain.byteLength;
		if (this.#size <= DEFAULT_MAX_FRAME_SIZE) return;

		// The bound never shrinks on its own, so once it passes the limit measure the value itself:
		// deletions may have freed room. That resets the bound to the real size, so the measurement
		// repeats only after another `limit - size` bytes of patches, not per delta.
		this.#size = new TextEncoder().encode(JSON.stringify(this.#current)).byteLength;
		if (this.#size > DEFAULT_MAX_FRAME_SIZE) {
			this.#current = undefined;
			throw new Error(`reconstructed value exceeded ${DEFAULT_MAX_FRAME_SIZE} bytes`);
		}
	}

	/**
	 * A copy of the reconstructed value as raw JSON, or `undefined` before the first snapshot.
	 *
	 * Copied rather than shared: it is the base every later {@link delta} merges into, so a caller
	 * mutating it would change fields no frame ever carried.
	 */
	get value(): unknown {
		return this.#current === undefined ? undefined : structuredClone(this.#current);
	}

	/**
	 * Materialize the reconstructed value, or `undefined` before the first snapshot.
	 *
	 * The result is the caller's own copy, for the same reason as {@link value}. (A schema already
	 * produces a fresh object, so it only needs cloning without one. The Rust decoder deserializes a
	 * fresh `T` every call and never exposes its internal state at all.)
	 */
	decode(): T | undefined {
		if (this.#current === undefined) return undefined;
		return this.#schema ? this.#schema.parse(this.#current) : (structuredClone(this.#current) as T);
	}

	// Decompress a frame against the group's window when compressing.
	#inflate(payload: Uint8Array): Uint8Array {
		return this.#flate ? this.#flate.frame(payload) : payload;
	}

	#parse(plain: Uint8Array): unknown {
		return JSON.parse(new TextDecoder().decode(plain));
	}
}

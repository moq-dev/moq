import { Decoder as Flate } from "@moq/flate";

import { merge } from "../diff.ts";
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

	constructor(config: Config<T> = {}) {
		this.#schema = config.schema;
		this.#decompress = config.compression ?? false;
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
		this.#current = this.#parse(payload);
	}

	/**
	 * Apply one of a group's later frames: an RFC 7396 merge patch against the current value.
	 *
	 * Throws when no snapshot has been applied yet, since a patch has nothing to apply to.
	 */
	delta(payload: Uint8Array): void {
		if (this.#current === undefined) throw new Error("delta before snapshot");
		this.#current = merge(this.#current, this.#parse(payload));
	}

	/** The reconstructed value as raw JSON, or `undefined` before the first snapshot. */
	get value(): unknown {
		return this.#current;
	}

	/** Materialize the reconstructed value, or `undefined` before the first snapshot. */
	decode(): T | undefined {
		if (this.#current === undefined) return undefined;
		return this.#schema ? this.#schema.parse(this.#current) : (this.#current as T);
	}

	// Decompress a frame against the group's window when compressing, then parse it as JSON.
	#parse(payload: Uint8Array): unknown {
		const plain = this.#flate ? this.#flate.frame(payload) : payload;
		return JSON.parse(new TextDecoder().decode(plain));
	}
}

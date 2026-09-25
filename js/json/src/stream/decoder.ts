import { Decoder as Flate } from "@moq/flate";

import { isDeflate } from "../compression.ts";
import type { Config } from "./encoder.ts";

/**
 * Decodes JSON records from frame payloads, sharing one DEFLATE window across the log.
 *
 * The track-free core of {@link Consumer}, and the mirror of {@link Encoder}. Payloads must be fed
 * in the order they were encoded, since each one builds on the window the earlier ones left behind.
 * Call {@link reset} at a group boundary, matching the encoder.
 */
export class Decoder<T> {
	#schema?: Config<T>["schema"];
	#decompress: boolean;
	// The DEFLATE window for the whole log, present while decompressing.
	#flate?: Flate;

	constructor(config: Config<T> = {}) {
		this.#schema = config.schema;
		this.#decompress = isDeflate(config.compression);
		this.#flate = this.#decompress ? new Flate() : undefined;
	}

	/** Start a cold DEFLATE window, for a caller that has just moved to a new group. */
	reset(): void {
		this.#flate = this.#decompress ? new Flate() : undefined;
	}

	/** Decode the next frame payload back into a record. */
	decode(payload: Uint8Array): T {
		const plain = this.#flate ? this.#flate.frame(payload) : payload;
		const value = JSON.parse(new TextDecoder().decode(plain));
		return this.#schema ? this.#schema.parse(value) : value;
	}
}

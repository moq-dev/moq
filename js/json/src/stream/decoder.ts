import { Decoder as Flate } from "@moq/flate";

/** Options shared by a {@link Decoder} and the {@link Consumer} that wraps one. */
export interface ConsumerConfig {
	/** Whether the frames are `deflate-raw` compressed. Must match the encoder. Defaults to `false`. */
	compression?: boolean;
}

/**
 * Decodes JSON records from frame payloads, sharing one DEFLATE window across the log.
 *
 * The track-free core of {@link Consumer}, and the mirror of {@link Encoder}. Payloads must be fed
 * in the order they were encoded, since each one builds on the window the earlier ones left behind.
 * Call {@link reset} at a group boundary, matching the encoder.
 */
export class Decoder<T> {
	#decompress: boolean;
	// The DEFLATE window for the whole log, present while decompressing.
	#flate?: Flate;

	constructor(config: ConsumerConfig = {}) {
		this.#decompress = config.compression ?? false;
		this.#flate = this.#decompress ? new Flate() : undefined;
	}

	/** Start a cold DEFLATE window, for a caller that has just moved to a new group. */
	reset(): void {
		this.#flate = this.#decompress ? new Flate() : undefined;
	}

	/** Decode the next frame payload back into a record. */
	decode(payload: Uint8Array): T {
		const plain = this.#flate ? this.#flate.frame(payload) : payload;
		return JSON.parse(new TextDecoder().decode(plain));
	}
}

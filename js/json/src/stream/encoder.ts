import { Encoder as Flate } from "@moq/flate";

/** Options shared by an {@link Encoder} and the {@link Producer} that wraps one. */
export interface ProducerConfig {
	/**
	 * Compress the group as one sync-flushed `deflate-raw` stream, so each record reuses the earlier
	 * ones as context and shrinks sharply. A {@link Decoder} reading the frames must set the same
	 * flag. Defaults to `false`.
	 */
	compression?: boolean;
}

/**
 * Encodes JSON records into frame payloads, sharing one DEFLATE window across the log.
 *
 * The track-free core of {@link Producer}. Unlike the `Snapshot` encoder there are no group
 * boundaries to report: a log is an unbroken sequence of self-contained records, so every payload is
 * simply the next frame.
 *
 * The window spans everything encoded so far, so payloads must reach the wire in order and be
 * decoded in the same order. If the caller does roll a group, call {@link reset} so the next record
 * starts a cold window that the new group's decoder can follow.
 */
export class Encoder<T> {
	#compress: boolean;
	// The DEFLATE window for the whole log, present while compressing.
	#flate?: Flate;

	constructor(config: ProducerConfig = {}) {
		this.#compress = config.compression ?? false;
		this.#flate = this.#compress ? new Flate() : undefined;
	}

	/** Start a cold DEFLATE window, for a caller that has just rolled a group. */
	reset(): void {
		this.#flate = this.#compress ? new Flate() : undefined;
	}

	/** Encode one record into the next frame payload. */
	encode(value: T): Uint8Array {
		const payload = new TextEncoder().encode(JSON.stringify(value));
		return this.#flate ? this.#flate.frame(payload) : payload;
	}
}

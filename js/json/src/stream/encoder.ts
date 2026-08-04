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
 * An encoded record the caller has not yet acknowledged writing, returned by {@link Encoder.encode}.
 *
 * Write the {@link payload}, then {@link commit}.
 *
 * A record that is never committed never reached the wire. With compression on that is
 * unrecoverable within the group: the window is ahead of what the consumer holds, and a log has no
 * keyframe to resynchronize on the way `Snapshot` does. So the encoder throws on the next
 * {@link Encoder.encode} until the caller rolls a new group and calls {@link Encoder.reset}. Without
 * compression each record stands alone, so a dropped one leaves a gap in the log but nothing
 * undecodable, and encoding continues.
 */
export interface Pending {
	/** The frame payload to write. */
	payload: Uint8Array;

	/**
	 * Acknowledge that the record reached the wire, keeping the encoder's window.
	 *
	 * Only call this once the write has actually succeeded.
	 */
	commit(): void;
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

	// Set when a compressed record was encoded but never written. The window is then ahead of the
	// consumer for the rest of the group, so encoding stops until the caller rolls a new one.
	#desynced = false;
	// Whether the record from the last {@link encode} is still unacknowledged.
	#pending = false;

	constructor(config: ProducerConfig = {}) {
		this.#compress = config.compression ?? false;
		this.#flate = this.#compress ? new Flate() : undefined;
	}

	/**
	 * Start a cold DEFLATE window, for a caller that has just rolled a group.
	 *
	 * This is also how a caller clears a desync: roll a new group so the consumer starts its own cold
	 * window, then reset.
	 */
	reset(): void {
		this.#flate = this.#compress ? new Flate() : undefined;
		this.#desynced = false;
		this.#pending = false;
	}

	/**
	 * Encode one record into the next frame payload.
	 *
	 * The record comes back as a {@link Pending} the caller writes and then commits. Throws if a
	 * previous compressed record was left uncommitted, since every frame after it would be
	 * undecodable.
	 */
	encode(value: T): Pending {
		// An uncompressed record carries no shared state, so losing one leaves a gap in the log rather
		// than an undecodable stream, and encoding continues.
		if (this.#pending && this.#compress) this.#desynced = true;
		if (this.#desynced) {
			throw new Error("compression desynchronized: a record was encoded but never written");
		}

		const bytes = new TextEncoder().encode(JSON.stringify(value));
		const payload = this.#flate ? this.#flate.frame(bytes) : bytes;

		this.#pending = true;
		return {
			payload,
			commit: () => {
				this.#pending = false;
			},
		};
	}
}

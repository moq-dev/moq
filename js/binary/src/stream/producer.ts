import { Encoder as Flate } from "@moq/flate";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";

/** Options for a {@link Producer}. */
export interface ProducerConfig {
	/**
	 * Compress the group as one sync-flushed `deflate-raw` stream, so each payload reuses the
	 * earlier ones as context. A {@link Consumer} reading the frames must set the same flag.
	 * Defaults to `false`.
	 */
	compression?: boolean;
}

/**
 * Publishes an ordered log of binary payloads to a track, one payload per frame in a single group.
 */
export class Producer {
	#track: Moq.Track.Producer;
	#compress: boolean;

	// The DEFLATE window for the whole log, present while compressing.
	#flate?: Flate;
	// The single group carrying the whole log, opened on the first append and never rolled.
	#group?: Moq.Group.Producer;

	/** Wrap a track to publish a payload log into it. */
	constructor(track: Moq.Track.Producer, config: ProducerConfig = {}) {
		this.#track = track;
		this.#compress = config.compression ?? false;
		this.#flate = this.#compress ? new Flate() : undefined;
	}

	/**
	 * Append one payload to the log.
	 *
	 * A payload that cannot be written ends the track: a log missing a record is not the lossless
	 * log this mode promises, so the failure is surfaced rather than papered over with a second
	 * group. Every later append then fails on the closed track.
	 */
	append(payload: Uint8Array): void {
		// Open the group before compressing: a failure here must not leave the window ahead of a
		// consumer that never received the frame.
		this.#group ??= this.#track.appendGroup();

		const encoded = this.#flate ? this.#flate.frame(payload) : payload;

		try {
			this.#group.writeFrame({ payload: encoded, timestamp: Time.Timestamp.now() });
		} catch (err) {
			// The payload never reached the wire, so the log has a hole in it, which is not the
			// lossless log this mode promises. Continuing into a second group would hand consumers a
			// gap dressed up as a complete log, so end the track and let the caller start a new one.
			//
			// The group is already visible on the track, so leaving it open would strand a subscriber
			// that advanced into it. Close both explicitly.
			this.finish();
			throw err;
		}
	}

	/** Finish the track, closing the group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}
}

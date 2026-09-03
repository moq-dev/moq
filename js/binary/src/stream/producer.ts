import { DEFAULT_MAX_FRAME_SIZE, Encoder as Flate } from "@moq/flate";
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
	 * group. The group is aborted rather than closed cleanly, so a consumer sees the failure
	 * instead of a log that merely looks complete. Every later append fails on the closed track.
	 */
	append(payload: Uint8Array): void {
		// A payload no consumer could decode is as terminal as one the track rejects: consumers all
		// decode with `@moq/flate`'s default cap, so this would publish a record none of them could
		// read. Ends the track like any other lost record, and aborts the group the same way the
		// write path below does: an earlier append may already have opened one, and leaving the two
		// paths to tear down differently only invites a reader to wonder which is authoritative.
		if (this.#flate && payload.byteLength > DEFAULT_MAX_FRAME_SIZE) {
			const err = new Error(`payload larger than the decoder's ${DEFAULT_MAX_FRAME_SIZE} byte limit`);
			this.#group?.close(err);
			this.#group = undefined;
			this.#track.close(err);
			throw err;
		}

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
			// Abort rather than closing cleanly: a clean close reads as `undefined`, exactly what a
			// completed log looks like, so a consumer could not tell a truncated log from a whole one.
			// Both halves are needed. The track reaches a reader that has not pulled the group yet,
			// since aborting only the group drops it from the cache and that reader still sees a clean
			// end. The group reaches a reader already inside it, which keeps its own handle and would
			// otherwise get a generic error when ours is dropped.
			const abort = err instanceof Error ? err : new Error(String(err));
			this.#group?.close(abort);
			this.#group = undefined;
			this.#track.close(abort);
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

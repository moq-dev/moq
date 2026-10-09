import type * as Moq from "@moq/net";
import { Group, Error as NetError, Time } from "@moq/net";
import { Encoder as Flate } from "../codec.ts";

import { type Compression, isDeflate } from "../compression.ts";

/** Codec options for a stream track. */
export interface Config {
	/**
	 * Compress the group as one sync-flushed `deflate-raw` stream, so each payload reuses the
	 * earlier ones as context. A {@link Consumer} reading the frames must set the same
	 * {@link compression}. Defaults to `"none"`.
	 */
	compression?: Compression;
}

/**
 * Publishes an ordered log of opaque payloads to a track, one payload per frame in a single group.
 */
export class Producer {
	#track: Moq.Track.Producer;
	#compress: boolean;

	// The DEFLATE window for the whole log, present while compressing.
	#flate?: Flate;
	// The single group carrying the whole log, opened on the first append and never rolled.
	#group?: Moq.Group.Producer;
	// Frames and payload bytes written to the group, checked against the group budget before each
	// payload is encoded.
	#frames = 0;
	#bytes = 0;

	/** Wrap a track to publish a payload log into it. */
	constructor(config: Producer.Config) {
		this.#track = config.track;
		this.#compress = isDeflate(config.compression);
		this.#flate = this.#compress ? new Flate() : undefined;
	}

	/**
	 * Append one payload to the log.
	 *
	 * A payload that might not fit in what is left of the group's budget throws `GroupTooLarge`
	 * before anything is written, leaving the log intact. The budget covers the whole log, so once
	 * it is spent every append throws; a publisher with more to say opens a new track.
	 *
	 * Any other payload that cannot be written ends the track: a log missing a record is not the
	 * lossless log this mode promises, so the failure is surfaced rather than papered over with a
	 * second group. The group is aborted rather than closed cleanly, so a consumer sees the failure
	 * instead of a log that merely looks complete. Every later append fails on the closed track.
	 *
	 * `at` is when the payload was captured, written as its frame timestamp. Defaults to now.
	 */
	append(payload: Uint8Array, at: Time.Timestamp = Time.Timestamp.now()): void {
		// Check before compressing: encoding advances the window, so a payload refused afterwards
		// would leave the encoder ahead of every reader. The worst case is checked rather than the
		// actual size for the same reason. Also checked before the group is opened, so a refused
		// first payload publishes nothing. The budget is below the decoder's cap, so any payload that
		// fits is one every consumer can inflate.
		const bound = this.#flate ? Flate.bound(payload.byteLength) : payload.byteLength;
		if (this.#frames >= Group.MAX_GROUP_FRAMES || this.#bytes + bound > Group.MAX_GROUP_CACHE_BYTES) {
			throw new NetError.GroupTooLarge();
		}

		// Open the group before compressing: a failure here must not leave the window ahead of a
		// consumer that never received the frame.
		this.#group ??= this.#track.appendGroup();

		const encoded = this.#flate ? this.#flate.frame(payload) : payload;

		try {
			this.#group.writeFrame({ payload: encoded, timestamp: at });
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

		this.#frames++;
		this.#bytes += encoded.byteLength;
	}

	/** Finish the track, closing the group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}
}

type Init = Config & { track: Moq.Track.Producer };

export namespace Producer {
	/** Stream producer options, including the destination track. */
	export type Config = Init;
}

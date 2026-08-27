import { Encoder as Flate } from "@moq/flate";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";

/** Options for a {@link Producer}. */
export interface ProducerConfig {
	/**
	 * Compress each value as its own raw DEFLATE stream.
	 *
	 * A snapshot group holds a single self-contained value, so there is no window to share: each
	 * value is compressed alone. A {@link Consumer} reading the frames must set the same flag.
	 * Defaults to `false`.
	 */
	compression?: boolean;
}

/**
 * Publishes a binary value to a track, one value per group.
 *
 * Each {@link update} rolls a new group holding the whole value, so a consumer only ever needs the
 * newest group and older ones are dropped. For a log where every payload survives, use the `Stream`
 * module.
 */
export class Producer {
	#track: Moq.Track.Producer;
	#compress: boolean;

	/** Wrap a track to publish a binary value into it. */
	constructor(track: Moq.Track.Producer, config: ProducerConfig = {}) {
		this.#track = track;
		this.#compress = config.compression ?? false;
	}

	/**
	 * Publish a new value, superseding the previous one.
	 *
	 * Unlike `@moq/json`, an identical value is republished rather than skipped: comparing two
	 * opaque blobs costs a full scan, and only the caller knows whether its bytes changed.
	 */
	update(payload: Uint8Array): void {
		// One frame per group, so the window spans a single value and starts cold every time.
		const encoded = this.#compress ? new Flate().frame(payload) : payload;

		const group = this.#track.appendGroup();
		try {
			group.writeFrame({ payload: encoded, timestamp: Time.Timestamp.now() });
		} finally {
			// The group is already visible on the track, so leaving it open on a failed write would
			// strand a subscriber that advanced into it with nothing to read and no end.
			group.close();
		}
	}

	/** Finish the track. */
	finish(): void {
		this.#track.close();
	}
}

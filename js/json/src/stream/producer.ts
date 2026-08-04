import type * as Moq from "@moq/net";
import { Time } from "@moq/net";

import { Encoder, type ProducerConfig } from "./encoder.ts";

/**
 * Publishes an ordered log of JSON records to a track, one record per frame in a single group.
 *
 * An {@link Encoder} that owns its track. When something else already owns the track, use the
 * {@link Encoder} directly.
 */
export class Producer<T> {
	#track: Moq.Track.Producer;
	#encoder: Encoder<T>;

	// The single group carrying the whole log, opened on the first append.
	#group?: Moq.Group.Producer;

	/** Wrap a track to publish a record log into it. */
	constructor(track: Moq.Track.Producer, config: ProducerConfig = {}) {
		this.#track = track;
		this.#encoder = new Encoder(config);
	}

	/** Append one record to the log. */
	append(value: T): void {
		// Open the group before encoding. Encoding folds the record into the DEFLATE window, so a
		// failure here would leave the window carrying a record that never reached the wire and every
		// later frame would decode against context the consumer doesn't have.
		this.#group ??= this.#track.appendGroup();
		this.#group.writeFrame({ payload: this.#encoder.encode(value), timestamp: Time.Timestamp.now() });
	}

	/** Finish the track, closing the group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}
}

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
		// Encode first, so a value that can't be serialized doesn't publish an empty group that
		// subscribers would advance into and wait on. Opening the group afterwards is safe because
		// the record stays uncommitted until the write lands.
		const record = this.#encoder.encode(value);

		try {
			this.#group ??= this.#track.appendGroup();
			this.#group.writeFrame({ payload: record.payload, timestamp: Time.Timestamp.now() });
		} catch (err) {
			// The group is already visible on the track, so leaving it open would strand a subscriber
			// that advanced into it. Closing it is also the group roll that recovery needs, so reset the
			// encoder to finish it: the record never landed, and without the reset a compressed encoder
			// refuses every later one even though the fresh group could carry it.
			this.#group?.close();
			this.#group = undefined;
			this.#encoder.reset();
			throw err;
		}
		record.commit();
	}

	/** Finish the track, closing the group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}
}

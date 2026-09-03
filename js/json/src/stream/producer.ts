import type * as Moq from "@moq/net";
import { Time } from "@moq/net";

import { Encoder, type Pending, type ProducerConfig } from "./encoder.ts";

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

	/**
	 * Append one record to the log.
	 *
	 * A record that cannot be written ends the track: a log missing a record is not the lossless log
	 * this mode promises, so the failure is surfaced rather than papered over with a second group.
	 * The track is aborted rather than closed cleanly, so a consumer sees the failure instead of a
	 * log that merely looks complete.
	 */
	append(value: T): void {
		// Encode first, so a value that can't be serialized doesn't publish an empty group that
		// subscribers would advance into and wait on. Opening the group afterwards is safe because
		// the record stays uncommitted until the write lands.
		let record: Pending;
		try {
			record = this.#encoder.encode(value);
		} catch (err) {
			// A record that can't be encoded is as lost as one the group rejects: the log is missing it
			// either way. Nothing was published, so this only has to end the track.
			this.#abort(err);
			throw err;
		}

		// Split the two failures: what decides whether this is terminal is whether a group is live by
		// the time the write fails, not whether one was already open when the append started. The
		// first append opens group 0 and can then fail its write, which is just as terminal.
		try {
			this.#group ??= this.#track.appendGroup();
		} catch (err) {
			// Nothing was published, so a later append may still open a fresh group whose decoder
			// starts cold. The record never landed, so the window still has to reset, or the desync
			// latch answers the next append before the track does.
			this.#encoder.reset();
			throw err;
		}

		try {
			this.#group.writeFrame({ payload: record.payload, timestamp: Time.Timestamp.now() });
		} catch (err) {
			// The group is live, so the record is a hole in the log and a second group would hand
			// consumers that gap dressed up as a complete log.
			this.#encoder.reset();
			this.#abort(err);
			throw err;
		}

		record.commit();
	}

	// End the track with an error, so a consumer sees the failure rather than a clean end.
	#abort(err: unknown): void {
		const abort = err instanceof Error ? err : new Error(String(err));

		// Close the group with the same error first. A consumer that already pulled it holds its own
		// handle, which the track's close no longer reaches, so dropping ours would leave a reader
		// sitting in the group with a generic error instead of the failure that ended the log.
		this.#group?.close(abort);
		this.#group = undefined;
		this.#track.close(abort);
	}

	/** Finish the track, closing the group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}
}

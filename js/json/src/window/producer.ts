import type * as Moq from "@moq/net";
import { Time } from "@moq/net";

import { type Encoded, Encoder, type ProducerConfig } from "./encoder.ts";

/**
 * Publishes a sliding window of JSON records to a track.
 *
 * An {@link Encoder} that owns its track: it writes each encoded frame and rolls a group whenever
 * the encoder emits a header. When something else already owns the track, use the {@link Encoder}
 * directly.
 *
 * @public
 */
export class Producer<T> {
	#track: Moq.Track.Producer;
	#encoder: Encoder<T>;
	#finished = false;

	// The group an op would be appended to, open after a header.
	#group?: Moq.Group.Producer;

	/** Wrap a track to publish a window into it. */
	constructor(track: Moq.Track.Producer, config: ProducerConfig = {}) {
		this.#track = track;
		this.#encoder = new Encoder(config);
	}

	/** The retained checkpoint suffix, oldest first. This is complete unless bounded in the config. */
	get window(): T[] {
		return this.#encoder.window;
	}

	/** Absolute index of the oldest retained record. */
	get offset(): number {
		return this.#encoder.offset;
	}

	/** Append one record to the back of the window. */
	push(value: T): void {
		this.#assertOpen();
		this.#write(this.#encoder.push(value));
	}

	/**
	 * Drop `count` records from the front of the window.
	 *
	 * A no-op when the window is already empty, and clamped to what it holds, so a caller can trim
	 * unconditionally.
	 */
	pop(count: number): void {
		this.#assertOpen();
		const frame = this.#encoder.pop(count);
		if (frame) this.#write(frame);
	}

	#assertOpen(): void {
		if (this.#finished) throw new Error("track is closed");
	}

	#write(encoded: Encoded & { commit(): void }): void {
		// A throw leaves the edit uncommitted and the retained window unchanged. The next edit opens a
		// new group because the attempted frame advanced the group-local compression state.
		if (encoded.keyframe) {
			// The previous group is complete; no more frames will be appended to it. Drop the handle
			// before opening the next one, so a failure below doesn't leave a closed group behind.
			this.#group?.close();
			this.#group = undefined;

			const group = this.#track.appendGroup();
			try {
				group.writeFrame({ payload: encoded.payload, timestamp: Time.Timestamp.now() });
			} catch (err) {
				// The group carries no frames, so close it rather than leaving it open on the track. A
				// consumer that already advanced into it would otherwise block with nothing to read.
				group.close();
				throw err;
			}

			this.#group = group;
			encoded.commit();
			return;
		}

		if (!this.#group) throw new Error("op with no open group");
		this.#group.writeFrame({ payload: encoded.payload, timestamp: Time.Timestamp.now() });
		encoded.commit();
	}

	/** Finish the track, closing any open group. */
	finish(): void {
		if (this.#finished) return;
		this.#finished = true;

		this.#group?.close();
		this.#group = undefined;

		this.#track.close();
	}
}

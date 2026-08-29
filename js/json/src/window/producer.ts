import type * as Moq from "@moq/net";
import { Time } from "@moq/net";

import { type Encoded, Encoder, type ProducerConfig } from "./encoder.ts";

/**
 * Publishes a sliding window of JSON records to a track.
 *
 * An {@link Encoder} that owns its track: it writes each encoded frame and rolls a group whenever
 * the encoder emits a reset. When something else already owns the track, use the {@link Encoder}
 * directly.
 */
export class Producer<T> {
	#track: Moq.Track.Producer;
	#encoder: Encoder<T>;

	// The group an op would be appended to, open between resets.
	#group?: Moq.Group.Producer;

	/** Wrap a track to publish a window into it. */
	constructor(track: Moq.Track.Producer, config: ProducerConfig = {}) {
		this.#track = track;
		this.#encoder = new Encoder(config);
	}

	/** The retained window, oldest first. */
	get window(): T[] {
		return this.#encoder.window;
	}

	/** Absolute index of the oldest retained record. */
	get offset(): number {
		return this.#encoder.offset;
	}

	/** Append one record to the back of the window. */
	push(value: T): void {
		this.#write(this.#encoder.push(value));
	}

	/**
	 * Drop `count` records from the front of the window.
	 *
	 * A no-op when the window is already empty, and clamped to what it holds, so a caller can trim
	 * unconditionally.
	 */
	pop(count: number): void {
		const frame = this.#encoder.pop(count);
		if (frame) this.#write(frame);
	}

	#write(encoded: Encoded & { commit(): void }): void {
		// A throw here leaves the frame uncommitted, so the next edit restates the whole window. The
		// edit itself stands: the publisher's window really did change, and only the consumer's
		// knowledge of it is lost.
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
		this.#group?.close();
		this.#group = undefined;

		// The open group goes with the track, so the encoder must not keep emitting ops into it. Any
		// further edit fails on the closed track, but it has to fail as a track error rather than as an
		// op with nowhere to put it.
		this.#encoder.reset();
		this.#track.close();
	}
}

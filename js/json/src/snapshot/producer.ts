import type * as Moq from "@moq/net";
import { Time } from "@moq/net";

import { type Config, DEFAULT_DELTA_RATIO, type Encoded, Encoder } from "./encoder.ts";

/** Snapshot producer options, including the destination track. */
export type ProducerConfig<T> = Config<T> & { track: Moq.Track.Producer };

/**
 * Publishes a JSON value over a track, choosing snapshots and deltas automatically.
 *
 * An {@link Encoder} that owns its track: it writes each encoded frame and rolls a group whenever
 * the encoder emits a snapshot. When something else already owns the track, use the {@link Encoder}
 * directly.
 */
export class Producer<T> {
	#track: Moq.Track.Producer;
	#encoder: Encoder<T>;
	#initial?: T;

	// The group a delta would be appended to, open only while deltas are enabled.
	#group?: Moq.Group.Producer;
	// Whether the encoder can emit deltas at all. With them off every frame is a snapshot, so a group
	// is closed the moment it's written and never held open.
	#deltas: boolean;

	constructor(config: ProducerConfig<T>) {
		this.#track = config.track;
		this.#encoder = new Encoder(config);
		this.#initial = config.initial;
		this.#deltas = (config.deltaRatio ?? DEFAULT_DELTA_RATIO) !== 0;
	}

	/** Publish a new value, emitting a snapshot or delta automatically. No-op if unchanged. */
	update(value: T): void {
		const frame = this.#encoder.update(value);
		if (!frame) return;

		// A throw here leaves the frame uncommitted, so the next update resynchronizes with a fresh
		// snapshot. A delta against a snapshot no consumer ever saw would be unreadable.
		this.#write(frame);
		frame.commit();
	}

	#write(encoded: Encoded): void {
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
				// rejected frame doesn't close the track, and a consumer that already advanced into this
				// group would otherwise block until some later update opened a newer one.
				group.close();
				throw err;
			}

			if (this.#deltas) {
				// Keep the group open so future deltas can be appended.
				this.#group = group;
			} else {
				// Deltas disabled: one frame per group, identical to a plain JSON track.
				group.close();
				this.#group = undefined;
			}
			return;
		}

		if (!this.#group) throw new Error("delta with no open group");
		this.#group.writeFrame({ payload: encoded.payload, timestamp: Time.Timestamp.now() });
	}

	/**
	 * Mutate the current value in place and publish the result.
	 *
	 * The callback receives a deep clone of the last-published value, falling back to
	 * {@link Config.initial} if nothing has been published yet (throws if neither exists). Edit it in
	 * place; on return the result is published via {@link update}, a no-op if unchanged:
	 *
	 * ```ts
	 * producer.mutate((catalog) => {
	 * 	catalog.scte35 = { ... };
	 * });
	 * ```
	 *
	 * Independent owners can share a single Producer and each edit only their own keys: every call
	 * starts from the latest value, so sections compose instead of clobbering one another. Use
	 * {@link update} to replace the whole value instead.
	 */
	mutate(fn: (value: T) => void): void {
		// Start from the last-published value, falling back to the configured initial value. We
		// don't invent an empty object: mutating with nothing to start from is a usage error.
		const base = this.#encoder.value ?? this.#initial;
		if (base === undefined) {
			throw new Error("mutate() requires a prior update() or `initial` in the config");
		}

		const value = structuredClone(base) as T;
		fn(value);
		this.update(value);
	}

	/** Finish the track, closing any open group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}
}

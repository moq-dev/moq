import * as Moq from "@moq/net";
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

	/**
	 * Finish the open group, so the deltas already written stop being provisional.
	 *
	 * No replacement group opens until the next {@link update}, which emits a full snapshot as its
	 * first frame even when the value is unchanged. A consumer joining at that group therefore reads
	 * the whole value without the deltas that preceded it.
	 *
	 * Idempotent: cutting when no group is open does nothing, so a caller can cut on its own schedule
	 * without tracking what has been published since the last one. Inert when deltas are disabled,
	 * where every frame already gets its own group.
	 */
	cut(): void {
		if (!this.#group) return;

		// Reset first: the group closes either way below, and a throw must not leave the encoder
		// emitting deltas against a snapshot whose group is gone.
		this.#encoder.reset();

		const group = this.#group;
		this.#group = undefined;
		group.close();
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
		// Check before touching a group. A keyframe closes the previous group and publishes its
		// replacement before the frame is written, so discovering the limit inside `writeFrame` would
		// leave an empty newest group behind: a snapshot consumer jumps to the newest, so the last
		// good value would vanish even though this update reported an error.
		if (encoded.payload.byteLength > Moq.Group.MAX_GROUP_CACHE_BYTES) throw new Moq.Group.FrameTooLarge();

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
	 * The callback receives a deep clone of the current value: everything published through this
	 * producer so far, composed, falling back to {@link Config.initial} if nothing has been (throws if
	 * neither exists). Edit it in place; on return the result is published via {@link update}, a no-op
	 * if unchanged:
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
	 *
	 * After a rejected write the current value is what the producer last *tried* to publish, which
	 * consumers never received. That is deliberate: dropping a rejected owner's field would clobber it
	 * for whoever edits next, which is the failure this API exists to prevent. The owner whose write
	 * failed sees the error, and the next successful publish is a full snapshot carrying the composed
	 * value, so consumers converge on it either way.
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

		// The open group goes with the track, so the encoder must not keep emitting deltas into it.
		// Any further update fails on the closed track, but it has to fail as a track error rather
		// than as a delta with nowhere to put it.
		this.#encoder.reset();
		this.#track.close();
	}
}

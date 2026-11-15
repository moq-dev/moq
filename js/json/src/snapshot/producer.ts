import type { Time, Timed } from "@moq/net";
import * as Moq from "@moq/net";
import { MAX_FRAME_BYTES } from "../limits.ts";
import { frameTime, openGroup, startsPast } from "../time.ts";

import { type Config as CodecConfig, type Encoded, Encoder } from "./encoder.ts";

/**
 * A value its producer finished: it takes no more updates. {@link Producer.finish} returns it,
 * mirroring the Rust `snapshot::Finished`; the producer itself still refuses an update after the
 * finish.
 */
// biome-ignore lint/suspicious/noEmptyInterface: nothing is left to call once the track is released.
export interface Finished {}

/**
 * Publishes a JSON value over a track, choosing snapshots and deltas automatically.
 *
 * An {@link Encoder} that owns its track: it writes each encoded frame and rolls a group whenever
 * the encoder emits a snapshot. When something else already owns the track, use the {@link Encoder}
 * directly.
 *
 * A group of deltas runs until a joiner wants a fresh one: once the deltas outweigh the track's
 * `refresh` ratio times the snapshot, a join asks for a group past the largest, and the producer
 * restates the value there at the newest timestamp written. On a scale-less track that timestamp
 * names the group, so while it is the newest group's own key the refresh waits for a later update,
 * since two groups never share a key. Set the ratio when creating the track, or joins never ask.
 *
 * Set `compression` on that track's info too: JSON is small and repetitive, and moq-net compresses each
 * delta against the snapshot earlier in its group.
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

	// The sequence of the newest group written, which a joiner's wanted group has to pass.
	#largest?: bigint;
	// The newest timestamp written, which a refresh restates the value at. An update may carry an
	// older timestamp than its group's first frame, and a new group may not start before that one.
	// Undefined before the first write.
	#at?: Time.Timestamp;
	// Stops watching the track's demand.
	#unwatch?: () => void;
	// A scale-less refresh that waits for a key past the newest group's, which the next write answers.
	#deferred = false;

	constructor(config: Producer.Config<T>) {
		this.#track = config.track;
		this.#encoder = new Encoder(config);
		this.#initial = config.initial;
		this.#deltas = config.deltas !== false;

		// With deltas off every group holds one frame, so no join ever asks for a fresh one.
		if (this.#deltas) this.#unwatch = this.#track.demand().wanted.subscribe((wanted) => this.#refresh(wanted));
	}

	// Restate the value in a new group once a joiner wants one past the largest written.
	#refresh(wanted: bigint | undefined): void {
		this.#deferred = false;
		if (wanted === undefined || this.#largest === undefined || wanted <= this.#largest) return;
		// Two groups never share a key, so a scale-less refresh at the newest group's own key waits for
		// a later write. Checked before encoding, since a snapshot dropped unwritten resets the encoder.
		const at = this.#at;
		if (at === undefined || !startsPast(this.#track, at)) {
			this.#deferred = true;
			return;
		}
		const frame = this.#encoder.snapshot();
		if (!frame) return;

		// Nobody awaits a refresh, so a failed write has no caller to report to. Abort the track rather
		// than leave its newest group without the value, where a latest-value subscriber would wait.
		try {
			this.#write(frame, at);
			frame.commit();
		} catch (err) {
			this.#close(err instanceof Error ? err : new Error(String(err)));
		}
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
		group.finish();
	}

	/**
	 * Publish a new value, emitting a snapshot or delta automatically. No-op if unchanged.
	 *
	 * `at` is when the value was captured, written as its frame timestamp. On a scale-less track it
	 * is {@link Time.Timestamp.unscaled} at a group key instead: a snapshot opens its group there,
	 * which must pass every group the track holds, and a delta carries the open group's key.
	 */
	update({ value, at }: Timed<T>): void {
		const frame = this.#encoder.update(value);
		if (!frame) return;

		// A throw here leaves the frame uncommitted, so the next update resynchronizes with a fresh
		// snapshot. A delta against a snapshot no consumer ever saw would be unreadable. The value
		// stays the last one written, so a refresh restates that rather than the rejection.
		this.#write(frame, at);
		frame.commit();
		if (this.#at === undefined || this.#at.isBefore(at)) this.#at = at;
		// A deferred refresh waits for a key past the open group's, which this may be.
		if (this.#deferred) this.#refresh(this.#track.demand().wanted.peek());
	}

	#write(encoded: Encoded, at: Time.Timestamp): void {
		// Check before touching a group. A keyframe closes the previous group and publishes its
		// replacement before the frame is written, so discovering the limit inside `writeFrame` would
		// leave an empty newest group behind: a snapshot consumer jumps to the newest, so the last
		// good value would vanish even though this update reported an error.
		if (encoded.payload.byteLength > MAX_FRAME_BYTES) throw new Moq.Error.FrameTooLarge();

		if (encoded.keyframe) {
			// Opened before the previous group finishes, so a key at or before the newest group's is
			// refused with the previous group still open.
			const group = openGroup(this.#track, at);
			this.#largest = group.sequence;

			// The previous group is complete; no more frames will be appended to it.
			this.#group?.finish();
			this.#group = undefined;
			try {
				group.writeFrame({ payload: encoded.payload, timestamp: at });
			} catch (err) {
				// The group carries no frames, so close it rather than leaving it open on the track. A
				// rejected frame doesn't close the track, and a consumer that already advanced into this
				// group would otherwise block until some later update opened a newer one.
				group.finish();
				throw err;
			}

			if (this.#deltas) {
				// Keep the group open so future deltas can be appended.
				this.#group = group;
			} else {
				// Deltas disabled: one frame per group, identical to a plain JSON track.
				group.finish();
				this.#group = undefined;
			}
			return;
		}

		if (!this.#group) throw new Error("delta with no open group");
		this.#group.writeFrame({ payload: encoded.payload, timestamp: frameTime(this.#group, at) });
	}

	/**
	 * Mutate the current value in place and publish the result, captured at `at` (see
	 * {@link update}).
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
	 * A rejected edit never reached the wire, so the current value stays the last one that did. The
	 * owner whose write failed sees the error and can retry; carrying the rejected edit forward would
	 * instead fail every later owner's publish the same way, or publish a value its owner was told
	 * failed.
	 */
	mutate(fn: (value: T) => void, at: Time.Timestamp): void {
		// Start from the last-published value, falling back to the configured initial value. We
		// don't invent an empty object: mutating with nothing to start from is a usage error.
		const base = this.#encoder.value ?? this.#initial;
		if (base === undefined) {
			throw new Error("mutate() requires a prior update() or `initial` in the config");
		}

		const value = structuredClone(base) as T;
		fn(value);
		this.update({ value, at });
	}

	/** Finish the track, closing any open group, and return it as {@link Finished}. */
	finish(): Finished {
		this.#close();
		return this;
	}

	#close(abort?: Error): void {
		this.#unwatch?.();
		this.#unwatch = undefined;
		if (abort) this.#group?.abort(abort);
		else this.#group?.finish();
		this.#group = undefined;

		// The open group goes with the track, so the encoder must not keep emitting deltas into it.
		// Any further update fails on the closed track, but it has to fail as a track error rather
		// than as a delta with nowhere to put it.
		this.#encoder.reset();
		if (abort) this.#track.abort(abort);
		else {
			this.#track.finish();
			this.#track.close();
		}
	}
}

/** Types for {@link Producer}. */
export namespace Producer {
	/** Snapshot producer options, including the destination track. */
	export type Config<T> = CodecConfig<T> & { track: Moq.Track.Producer };
}

import type * as Moq from "@moq/net";
import type { Time, Timed } from "@moq/net";

import { frameTime, openGroup, startsPast } from "../time.ts";
import { type Config, type Encoded, Encoder } from "./encoder.ts";

/** Window producer options, including the destination track. */
export type ProducerConfig = Config & { track: Moq.Track.Producer };

/**
 * A window its producer finished: its records can still be read, but not edited.
 * {@link Producer.finish} returns it, mirroring the Rust `window::Finished`; the producer itself
 * still refuses an edit after the finish.
 *
 * @public
 */
export interface Finished<T> {
	/** The retained checkpoint suffix, oldest first; see {@link Producer.window}. */
	readonly window: T[];
	/** Absolute index of the oldest retained record; see {@link Producer.offset}. */
	readonly offset: number;
	/** Abort the track with an error; see {@link Producer.abort}. */
	abort(err: Error): void;
}

/**
 * Publishes a sliding window of JSON records to a track.
 *
 * An {@link Encoder} that owns its track: it writes each encoded frame and rolls a group whenever
 * the encoder emits a header. When something else already owns the track, use the {@link Encoder}
 * directly.
 *
 * A group of ops runs until a joiner wants a fresh one: once the ops outweigh the track's `refresh`
 * ratio times the header, a join asks for a group past the largest, and the producer restates the
 * window there at the newest timestamp written. On a scale-less track that timestamp names the
 * group, so while it is the newest group's own key the refresh waits for a later edit, since two
 * groups never share a key. Set the ratio when creating the track, or joins never ask.
 *
 * @public
 */
export class Producer<T> {
	#track: Moq.Track.Producer;
	#encoder: Encoder<T>;
	#finished = false;

	// The group an op would be appended to, open after a header.
	#group?: Moq.Group.Producer;

	// The sequence of the newest group written, which a joiner's wanted group has to pass.
	#largest?: bigint;
	// The newest timestamp written, which a refresh restates the window at. An edit may carry an
	// older timestamp than its group's first frame, and a new group may not start before that one.
	// Undefined before the first write.
	#at?: Time.Timestamp;
	// Stops watching the track's demand.
	#unwatch?: () => void;
	// A scale-less refresh that waits for a key past the newest group's, which the next write answers.
	#deferred = false;

	/** Wrap a track to publish a window into it. */
	constructor(config: ProducerConfig) {
		this.#track = config.track;
		this.#encoder = new Encoder(config);

		// With ops off every group holds one frame, so no join ever asks for a fresh one.
		if (config.ops !== false) {
			this.#unwatch = this.#track.demand().wanted.subscribe((wanted) => this.#refresh(wanted));
		}
	}

	// Restate the window in a new group once a joiner wants one past the largest written.
	#refresh(wanted: bigint | undefined): void {
		this.#deferred = false;
		if (this.#finished || wanted === undefined || this.#largest === undefined || wanted <= this.#largest) return;
		// Two groups never share a key, so a scale-less refresh at the newest group's own key waits for
		// a later write. Checked before encoding, since a header dropped unwritten resets the encoder.
		const at = this.#at;
		if (at === undefined || !startsPast(this.#track, at)) {
			this.#deferred = true;
			return;
		}

		// Nobody awaits a refresh, so a failed write has no caller to report to. Abort the track rather
		// than leave its newest group without the window, where a joiner would wait.
		try {
			this.#write(this.#encoder.snapshot(), at);
		} catch (err) {
			this.#close(err instanceof Error ? err : new Error(String(err)));
		}
	}

	/** The retained checkpoint suffix, oldest first. This is complete unless bounded in the config. */
	get window(): T[] {
		return this.#encoder.window;
	}

	/** Absolute index of the oldest retained record. */
	get offset(): number {
		return this.#encoder.offset;
	}

	/**
	 * Append one record to the back of the window.
	 *
	 * `at` is when the record was captured, written as its frame timestamp. On a scale-less track it
	 * is {@link Time.Timestamp.unscaled} at a group key instead: a header opens its group there, which
	 * must pass every group the track holds, and an op carries the open group's key.
	 */
	push({ value, at }: Timed<T>): void {
		this.#assertOpen();
		this.#write(this.#encoder.push(value), at);
		this.#advance(at);
	}

	/**
	 * Drop `count` records from the front of the window.
	 *
	 * A no-op when the window is already empty, and clamped to what it holds, so a caller can trim
	 * unconditionally.
	 *
	 * `at` is when the records were retired, written as the frame timestamp (see {@link push}).
	 */
	pop(count: number, at: Time.Timestamp): void {
		this.#assertOpen();
		const frame = this.#encoder.pop(count);
		if (!frame) return;
		this.#write(frame, at);
		this.#advance(at);
	}

	// Remember `at` if it is the newest timestamp written, then answer a deferred refresh it may
	// unblock: a scale-less refresh waits for a key past the open group's.
	#advance(at: Time.Timestamp): void {
		if (this.#at === undefined || this.#at.isBefore(at)) this.#at = at;
		if (this.#deferred) this.#refresh(this.#track.demand().wanted.peek());
	}

	#assertOpen(): void {
		if (this.#finished) throw new Error("track is closed");
	}

	#write(encoded: Encoded & { commit(): void }, at: Time.Timestamp): void {
		// A throw leaves the edit uncommitted and the retained window unchanged. The next edit opens a
		// new group, since a consumer may hold the group the frame was meant for.
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
				// consumer that already advanced into it would otherwise block with nothing to read.
				group.finish();
				throw err;
			}

			this.#group = group;
			encoded.commit();
			return;
		}

		if (!this.#group) throw new Error("op with no open group");
		this.#group.writeFrame({ payload: encoded.payload, timestamp: frameTime(this.#group, at) });
		encoded.commit();
	}

	/**
	 * Finish the open group, leaving the next edit to open a replacement with a header.
	 *
	 * Idempotent: cutting when no group is open does nothing. A caller that stores complete groups
	 * cuts after its edits, so every edit so far sits in a group no later frame can extend.
	 */
	cut(): void {
		if (!this.#group) return;

		// Reset first: the group closes either way below, and a throw must not leave the encoder
		// appending ops to a group that is gone.
		this.#encoder.reset();

		const group = this.#group;
		this.#group = undefined;
		group.finish();
	}

	/** Finish the track, closing any open group, and return it as {@link Finished}. */
	finish(): Finished<T> {
		this.#close();
		return this;
	}

	/** Abort the track with `err`, so readers see the cause rather than a complete window. Idempotent. */
	abort(err: Error): void {
		this.#close(err);
	}

	#close(abort?: Error): void {
		if (this.#finished) return;
		this.#finished = true;
		this.#unwatch?.();
		this.#unwatch = undefined;

		if (abort) this.#group?.abort(abort);
		else this.#group?.finish();
		this.#group = undefined;

		if (abort) this.#track.abort(abort);
		else {
			this.#track.finish();
			this.#track.close();
		}
	}
}

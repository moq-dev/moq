import type * as Moq from "@moq/net";
import type { Time, Timed } from "@moq/net";

import { type Config, type Encoded, Encoder } from "./encoder.ts";

/** Window producer options, including the destination track. */
export type ProducerConfig = Config & { track: Moq.Track.Producer };

/**
 * Publishes a sliding window of JSON records to a track.
 *
 * An {@link Encoder} that owns its track: it writes each encoded frame and rolls a group whenever
 * the encoder emits a header. When something else already owns the track, use the {@link Encoder}
 * directly.
 *
 * A group of ops runs until a joiner wants a fresh one: once the ops outweigh the track's `refresh`
 * ratio times the header, a join asks for a group past the largest, and the producer restates the
 * window there at the newest timestamp written. Set the ratio when creating the track, or joins
 * never ask.
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
	#largest?: number;
	// The newest timestamp written, which a refresh restates the window at. An edit may carry an
	// older timestamp than its group's first frame, and a new group may not start before that one.
	#at?: Time.Timestamp;
	// Stops watching the track's demand.
	#unwatch?: () => void;

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
	#refresh(wanted: number | undefined): void {
		if (this.#finished || wanted === undefined || this.#largest === undefined || wanted <= this.#largest) return;

		// Nobody awaits a refresh, so a failed write has no caller to report to. Abort the track rather
		// than leave its newest group without the window, where a joiner would wait.
		try {
			this.#write(this.#encoder.snapshot(), this.#at);
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
	 * `at` is when the record was captured, written as its frame timestamp. Omit it only on an
	 * untimed track: it is never filled in.
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
	 * `at` is when the records were retired, written as the frame timestamp. Omit it only on an
	 * untimed track: it is never filled in.
	 */
	pop(count: number, at?: Time.Timestamp): void {
		this.#assertOpen();
		const frame = this.#encoder.pop(count);
		if (!frame) return;
		this.#write(frame, at);
		this.#advance(at);
	}

	// Remember `at` if it is the newest timestamp written.
	#advance(at: Time.Timestamp | undefined): void {
		if (at && !at.isBefore(this.#at ?? at)) this.#at = at;
	}

	#assertOpen(): void {
		if (this.#finished) throw new Error("track is closed");
	}

	#write(encoded: Encoded & { commit(): void }, at: Time.Timestamp | undefined): void {
		// A throw leaves the edit uncommitted and the retained window unchanged. The next edit opens a
		// new group, since a consumer may hold the group the frame was meant for.
		if (encoded.keyframe) {
			// The previous group is complete; no more frames will be appended to it. Drop the handle
			// before opening the next one, so a failure below doesn't leave a closed group behind.
			this.#group?.finish();
			this.#group = undefined;

			const group = this.#track.appendGroup();
			this.#largest = group.sequence;
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
		this.#group.writeFrame({ payload: encoded.payload, timestamp: at });
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

	/** Finish the track, closing any open group. */
	finish(): void {
		this.#close();
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

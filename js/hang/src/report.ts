/**
 * The track a media report rides on: a publisher's stats or a viewer's feedback.
 * @module
 */

import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type * as z from "@zod/mini";

/** How often a reporter writes, in milliseconds: about once a second, as the hang draft asks. */
export const INTERVAL = 1000;

/** The report track's priority: below every media track, so a report never delays the media it describes. */
export const PRIORITY = -128;

// Deltas compress against the snapshot in their group, and a joiner asks for a fresh group once
// they outweigh eight snapshots. Matches the Rust CLI's report tracks.
const REFRESH = 8;

/**
 * One compressed snapshot track, written only while someone subscribes, so an unobserved report
 * never leaves the process.
 */
export class Writer<T> {
	/** The report track. */
	readonly track: Moq.Track.Producer;

	#producer: Json.Snapshot.Producer<T>;
	#closed = false;

	/** Create `name` on `broadcast`, validating each report against `schema` before it is written. */
	constructor(broadcast: Moq.Broadcast.Producer, name: string, schema?: z.ZodMiniType<T>) {
		this.track = broadcast.createTrack(name, { priority: PRIORITY, compress: true, refresh: REFRESH });
		this.#producer = new Json.Snapshot.Producer<T>({ track: this.track, schema });
	}

	/** Whether anyone subscribes now, which is when {@link update} writes. Observe `track.demand().wanted` to follow it. */
	get wanted(): boolean {
		return this.track.demand().wanted.peek() !== undefined;
	}

	/**
	 * Publish `value` when someone subscribes; an unchanged value writes nothing. A track that refuses
	 * a write would refuse every later one, so it is aborted with the cause, which its subscribers read.
	 */
	update(value: T): void {
		if (this.#closed || !this.wanted) return;
		try {
			this.#producer.update({ value });
		} catch (err) {
			console.warn("a report track failed; closing it", err);
			this.#closed = true;
			this.track.abort(err instanceof Error ? err : new Error(String(err)));
		}
	}

	/** End the track cleanly, keeping the last report for a late subscriber. Idempotent. */
	finish(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#producer.finish();
	}
}

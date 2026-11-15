/**
 * The track a media report rides on: a publisher's stats or a viewer's feedback.
 * @module
 */

import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import type * as z from "zod/mini";

/** How often a reporter writes, in milliseconds: about once a second, as the hang draft asks. */
export const INTERVAL = 1000;

// Below every media track, so a report never delays the media it describes.
const PRIORITY = -128;

// Deltas compress against the snapshot in their group, and a joiner asks for a fresh group once
// they outweigh eight snapshots. Matches the Rust CLI's report tracks.
const REFRESH = 8;

/**
 * One compressed snapshot track, written only while someone subscribes, so an unobserved report
 * never leaves the process. It takes `@moq/net`'s zero default max age: a joiner reads only the
 * newest group, which starts with a full snapshot.
 */
export class Writer<T, I = T> {
	/** The report track. */
	readonly track: Moq.Track.Producer;

	#producer: Json.Snapshot.Producer<T>;
	#schema: z.ZodMiniType<T, I>;
	#closed = false;
	// The report track is scale-less, so each write names the next group key: past every group a
	// snapshot opened, while a delta carries its open group's key whatever this names.
	#key = 0n;

	/**
	 * Create `name` on `broadcast`, parsing each report with `schema` before it is written, so a
	 * publisher passes the schema's input (plain numbers) and readers get its output.
	 */
	constructor(broadcast: Moq.Broadcast.Producer, name: string, schema: z.ZodMiniType<T, I>) {
		this.track = broadcast.createTrack(name, { priority: PRIORITY, compression: "deflate", refresh: REFRESH });
		this.#producer = new Json.Snapshot.Producer<T>({ track: this.track });
		this.#schema = schema;
	}

	/**
	 * Publish `value` when someone subscribes (observe `track.demand().wanted` to follow that); an
	 * unchanged value writes nothing. A track that refuses a write would refuse every later one, so it
	 * is aborted with the cause, which its subscribers read.
	 */
	update(value: I): void {
		if (this.#closed || this.track.demand().wanted.peek() === undefined) return;
		try {
			this.#producer.update({ value: this.#schema.parse(value), at: Moq.Time.Timestamp.unscaled(this.#key++) });
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

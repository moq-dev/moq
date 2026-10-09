/**
 * Reading a track's timeline: a track carrying one record per span of another track, mapping
 * its content time to the group and frame positions that carry it. A consumer can seek (or
 * build an HLS/DASH playlist) without downloading the media.
 *
 * Every track has its own timeline, named in the catalog's root {@link Catalog.Archive} entry.
 * A span usually holds whole groups; a group that outlives the publisher's maximum span is
 * split by frame. Spans are contiguous in position unless the track skipped group sequences.
 *
 * @module
 */

import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Time } from "@moq/net";
import * as z from "@zod/mini";
import type * as Catalog from "./catalog";
import { u53Schema } from "./catalog";

/** A frame position within a track: frame `frame` of group `group`. */
export interface Position {
	group: number;
	/** Omitted when zero, so a position at a group start reads as just the group. */
	frame?: number;
}

/**
 * One timeline record: a span of one track holding every frame from `start` (inclusive) to
 * `end` (exclusive). `pts` and `duration` are in the archive's timescale. An `end` at frame zero
 * of group `g` holds every frame of group `g - 1` and nothing of `g`.
 */
export interface Record {
	/** The record's number, consecutive within its track's timeline. */
	sequence: number;
	pts: number;
	duration: number;
	start: Position;
	end: Position;
	/** Whether the span starts with a keyframe. Omitted when true (the default). */
	keyframe?: boolean;
}

const PositionSchema = z.object({ group: u53Schema, frame: z.optional(u53Schema) });

const RecordSchema = z.looseObject({
	sequence: u53Schema,
	pts: u53Schema,
	duration: u53Schema,
	start: PositionSchema,
	end: PositionSchema,
	keyframe: z.optional(z.boolean()),
});

/** The conventional suffix appended to a track name to name its timeline track. */
export const SUFFIX = ".timeline.z";

/** A record with timestamps converted to microseconds. */
export type Entry = Omit<Record, "pts" | "duration"> & { pts: Time.Micro; duration: Time.Micro };

/** One change to the visible timeline window. */
export type Event = { push: { index: number; entry: Entry } } | { pop: Json.Window.Span } | { skip: Json.Window.Span };

/** Reads one track's timeline, as advertised by a catalog archive entry. */
export class Consumer implements AsyncIterable<Event> {
	readonly #track: Moq.Track.Subscriber;
	readonly #window: Json.Window.Consumer<Record>;
	readonly #timescale: number;

	private constructor(track: Moq.Track.Subscriber, archive: Catalog.Archive) {
		this.#track = track;
		if (!Number.isSafeInteger(archive.timescale) || archive.timescale <= 0) {
			throw new Error("invalid timeline timescale");
		}
		this.#timescale = archive.timescale;
		this.#window = new Json.Window.Consumer<Record>({ track, compression: true });
	}

	/** Subscribe to `track`'s timeline as named by the catalog's archive entry. Throws if it has none. */
	static subscribe(broadcast: Moq.Broadcast.Consumer, archive: Catalog.Archive, track: string): Consumer {
		const name = archive.timelines[track];
		if (name === undefined) throw new Error(`no timeline for track ${track}`);
		return new Consumer(broadcast.track(name).subscribe(), archive);
	}

	/** Get the next timeline event, or `undefined` when the track ends. */
	async next(): Promise<Event | undefined> {
		const event = await this.#window.next();
		if (event === undefined) return undefined;
		if ("push" in event) {
			const { index } = event.push;
			const value = RecordSchema.parse(event.push.value);
			if (value.sequence !== index) throw new Error("timeline record sequence differs from its index");
			const pts = this.#micros(value.pts);
			const duration = this.#micros(value.duration);
			return { push: { index, entry: { ...value, pts, duration } } };
		}
		return event;
	}

	#micros(units: number): Time.Micro {
		const value = (BigInt(units) * 1_000_000n) / BigInt(this.#timescale);
		if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("timeline timestamp overflow");
		return Number(value) as Time.Micro;
	}

	/** Iterate timeline events until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Event> {
		for (;;) {
			const event = await this.next();
			if (event === undefined) return;
			yield event;
		}
	}

	/** Close the timeline subscription. */
	close(): void {
		this.#track.close();
	}
}

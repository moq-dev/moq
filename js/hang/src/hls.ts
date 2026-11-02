/**
 * HLS segment records: where each of a rendition's HLS segments starts.
 *
 * A rendition advertises them with its catalog `hls` field, which names a track carrying one
 * {@link Record} per segment. A segment starts at a group boundary, so a record is the segment's
 * first group and pts. It runs to the next record: it holds every group up to the next record's
 * `group` and lasts until the next record's `pts`, unless it carries an explicit `duration`. A
 * zero `duration` marks where content stopped: that segment holds no media, and what follows it
 * is a discontinuity.
 *
 * Every `pts` and `duration` is in milliseconds. The track is an uncompressed `@moq/json` window.
 *
 * @module
 */

import type { Micro } from "@moq/clock";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import * as z from "@zod/mini";
import type * as Catalog from "./catalog";
import { u53Schema } from "./catalog";

/** The conventional suffix appended to a media track's name to name its HLS record track. */
export const SUFFIX = ".hls";

/** One HLS segment: its first group and pts, in milliseconds. */
export interface Record {
	/** The segment's number, consecutive within the track and equal to the record's window index. */
	sequence: number;
	/** The group the segment starts with. */
	group: number;
	/** The pts of the segment's first frame. */
	pts: number;
	/** The segment's duration, when it does not run to the next record's pts. Zero marks a stop. */
	duration?: number;
}

const RecordSchema = z.looseObject({
	sequence: u53Schema,
	group: u53Schema,
	pts: u53Schema,
	duration: z.optional(u53Schema),
});

/** A record with its times converted to microseconds. */
export type Entry = Omit<Record, "pts" | "duration"> & { pts: Micro; duration?: Micro };

/** One change to the visible record window. */
export type Event = { push: { index: number; entry: Entry } } | { pop: Json.Window.Span } | { skip: Json.Window.Span };

/** Reads one rendition's HLS records. */
export class Consumer implements AsyncIterable<Event> {
	readonly #track: Moq.Track.Subscriber;
	readonly #window: Json.Window.Consumer<Record>;

	private constructor(track: Moq.Track.Subscriber) {
		this.#track = track;
		this.#window = new Json.Window.Consumer<Record>({ track });
	}

	/** Subscribe to the records a rendition's `hls` field names. */
	static subscribe(broadcast: Moq.Broadcast.Consumer, hls: Catalog.Hls): Consumer {
		return new Consumer(broadcast.track(hls.track).subscribe());
	}

	/** Get the next event, or `undefined` when the track ends. Throws on a malformed record. */
	async next(): Promise<Event | undefined> {
		// Each record carries its own pts, so the frame timestamp adds nothing.
		const event = (await this.#window.next())?.value;
		if (event === undefined) return undefined;
		if ("push" in event) {
			const { index } = event.push;
			const value = RecordSchema.parse(event.push.value);
			if (value.sequence !== index) throw new Error("HLS record sequence differs from its index");
			const entry: Entry = { sequence: value.sequence, group: value.group, pts: micros(value.pts) };
			if (value.duration !== undefined) entry.duration = micros(value.duration);
			return { push: { index, entry } };
		}
		return event;
	}

	/** Iterate events until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Event> {
		for (;;) {
			const event = await this.next();
			if (event === undefined) return;
			yield event;
		}
	}

	/** Close the record subscription. */
	close(): void {
		this.#track.close();
	}
}

function micros(millis: number): Micro {
	const value = millis * 1000;
	if (!Number.isSafeInteger(value)) throw new Error("HLS record timestamp overflow");
	return value as Micro;
}

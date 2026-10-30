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
 * Every `pts` and `duration` is in milliseconds. The track is a compressed `@moq/json` window
 * whose groups restate the recent records.
 *
 * A {@link Recorder} writes one rendition's records from reports of the frames its media track
 * publishes, as `rs/moq-mux`'s does. Segments are cut on a grid of {@link Config.duration} (4s by
 * default) in pts: a group starts a segment when its pts falls in a later multiple of the duration
 * than the previous group's. Short groups (audio) pack into one segment per grid cell, and long ones
 * (video GOPs) get one each. A hint replaces the duration with its smallest multiple at or beyond
 * it, which is also the nominal duration declared; without one, the first segment declares it.
 * Each rendition cuts on its own grid and numbers its own records, sharing no state with the
 * others, so renditions whose keyframes share timestamps cut the same boundaries.
 *
 * Content stopping (a skipped group sequence, a discontinuity marker, or the end of the track)
 * publishes a zero-duration record where it stopped, so the segment before it ends there rather
 * than absorbing the pause.
 *
 * @module
 */

import { Micro } from "@moq/clock";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { type Getter, Signal } from "@moq/signals";
import * as z from "@zod/mini";
import type * as Catalog from "./catalog";
import { PRIORITY, u53, u53Schema } from "./catalog";

/** The conventional suffix appended to a media track's name to name its HLS record track. */
export const SUFFIX = ".hls";

/** The conventional HLS record track name for `track`: the name plus {@link SUFFIX}. */
export function defaultName(track: string): string {
	return `${track}${SUFFIX}`;
}

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

/** The default {@link Config.duration} (4 seconds). */
export const DEFAULT_DURATION = Micro(4_000_000);

// Recent records repeated when the window track rolls to a new group: one more than the 256
// segments an HLS edge lists, since a segment is listed only once the record after it arrives.
const CHECKPOINT_RECORDS = 257;

/** How a track's HLS segments are cut. */
export interface Config {
	/**
	 * The spacing of the pts grid segments are cut on. Defaults to {@link DEFAULT_DURATION}.
	 *
	 * A grid rather than a running length on purpose: renditions whose keyframes share timestamps
	 * cut the same boundaries without sharing any state.
	 */
	duration?: Micro;

	/** The application's nominal group duration, when known. */
	durationHint?: Micro;
}

// A pts in wire milliseconds, truncated as a timescale conversion does.
function wire(pts: Micro): number {
	return Math.floor(pts / 1000);
}

/**
 * Builds one track's records from reports of the frames it published.
 *
 * Reports arrive in group order: {@link frame} for each frame (or at least the first of each group),
 * {@link end} wherever the content is known to extend, and {@link stop} where it stops. Records
 * queue until {@link take} drains them.
 */
export class Segmenter {
	readonly #duration: Micro;
	// The grid's spacing: the hint's multiple, else the configured duration.
	readonly #grid: Micro;
	// Fixed nominal duration; absent until an unhinted track's first segment closes. Kept unrounded;
	// only the wire declaration rounds.
	#declared?: Micro;
	// The number the next record gets.
	#sequence = 0;
	// The open segment's start: absent before the first segment and after a stop.
	#open?: Micro;
	// The grid cell of the open segment's newest group.
	#cell = 0;
	// The newest reported group.
	#last?: number;
	// The newest reported content time in the open segment.
	#frontier?: Micro;
	#ready: Record[] = [];
	#closed = false;

	/** A segmenter numbering its records from zero. */
	constructor(config: Config = {}) {
		this.#duration = config.duration ?? DEFAULT_DURATION;
		const hint = config.durationHint;
		if (hint !== undefined && hint > 0) {
			this.#declared = Micro(hint * Math.max(1, Math.ceil(this.#duration / hint)));
		}
		this.#grid = this.#declared ?? this.#duration;
	}

	/** The fixed nominal segment duration, once known. */
	get declared(): Micro | undefined {
		return this.#declared;
	}

	// The grid cell `pts` falls in. A zero grid gives every distinct pts its own.
	#cellOf(pts: Micro): number {
		return this.#grid > 0 ? Math.floor(pts / this.#grid) : pts;
	}

	/**
	 * Report a frame of group `group`, presented at `pts`.
	 *
	 * Throws when `group` precedes the previous report's: segments hold whole groups in order, so that
	 * frame could never be listed.
	 */
	frame(group: number, pts: Micro): void {
		if (this.#closed) return;
		if (this.#last !== undefined) {
			if (group < this.#last) throw new Error(`HLS report for group ${group} after group ${this.#last}`);
			if (group !== this.#last) this.#group(group, this.#last, pts);
		} else {
			this.#begin(group, pts);
		}
		this.#last = group;
		this.#advance(pts);
	}

	// Group `group` starts at `pts`, after group `last`.
	#group(group: number, last: number, pts: Micro): void {
		const start = this.#open;
		if (start === undefined) {
			this.#begin(group, pts);
			return;
		}
		if (group !== last + 1) {
			// A skipped sequence stops the segment where its content reached, so the skipped time is
			// not absorbed into it and the reader never has to guess what lies between.
			this.#stopAt(last + 1);
			this.#begin(group, pts);
			return;
		}
		// Rust's segmenter also cuts on source-declared boundaries, held to a ceiling; nothing here
		// declares them, so the grid alone decides.
		const cell = this.#cellOf(pts);
		if (cell > this.#cell) {
			// A grid's first segment starts wherever the content does, so it may be short.
			this.#declared ??= Micro(Math.max(pts - start, this.#grid));
			this.#begin(group, pts);
		}
		this.#cell = cell;
	}

	// Start a segment with group `group` at `pts`.
	#begin(group: number, pts: Micro): void {
		this.#open = pts;
		this.#frontier = pts;
		this.#cell = this.#cellOf(pts);
		this.#ready.push({ sequence: this.#sequence++, group, pts: wire(pts) });
	}

	/**
	 * Report that the content extends to `pts`, without a frame starting there.
	 *
	 * A frame report says where content starts; the stop after the last segment would otherwise land
	 * at its last frame's start.
	 */
	end(pts: Micro): void {
		if (!this.#closed) this.#advance(pts);
	}

	// Content in the open segment reached `pts`.
	#advance(pts: Micro): void {
		if (this.#open !== undefined && (this.#frontier === undefined || pts > this.#frontier)) this.#frontier = pts;
	}

	/**
	 * Content stopped before group `group`: queue a zero-duration record where it reached.
	 *
	 * The next group starts a new segment. Nothing happens while no segment is open.
	 */
	stop(group: number): void {
		if (!this.#closed) this.#stopAt(group);
	}

	#stopAt(group: number): void {
		const start = this.#open;
		if (start === undefined) return;
		this.#open = undefined;
		// One stopped early says nothing about the cadence, so the target falls back to the grid.
		this.#declared ??= this.#grid;
		const end = this.#frontier ?? start;
		this.#frontier = undefined;
		this.#ready.push({ sequence: this.#sequence++, group, pts: wire(end), duration: 0 });
	}

	/** Stop after the newest group, then ignore every later report. */
	close(): void {
		if (this.#closed) return;
		if (this.#last !== undefined) this.#stopAt(this.#last + 1);
		this.#closed = true;
	}

	/** Take the records queued since the last call. */
	take(): Record[] {
		const ready = this.#ready;
		this.#ready = [];
		return ready;
	}
}

/**
 * Cuts one media track's segments and publishes their records onto its HLS record track.
 *
 * The records are an optional sidecar, so nothing here throws at the media: a report it cannot place
 * or a failed publish aborts the record track instead, and the media carries on.
 */
export class Recorder {
	/** The properties a record track is created with. */
	static readonly INFO: Partial<Moq.Track.Info> = {
		timescale: Moq.Time.Timescale.MILLI,
		priority: PRIORITY.catalog,
		// A joiner asks for a fresh group once the ops outweigh eight headers.
		refresh: 8,
		// Records are small, repetitive JSON.
		compress: true,
	};

	/** The record track. */
	readonly track: Moq.Track.Producer;

	readonly #segmenter: Segmenter;
	readonly #sink: Json.Window.Producer<Record>;
	readonly #declared = new Signal<Catalog.Hls | undefined>(undefined);
	#done = false;

	/**
	 * The `hls` field to advertise for the media track, once its nominal duration is known: the
	 * record track and that duration in whole milliseconds, rounded up.
	 */
	readonly hls: Getter<Catalog.Hls | undefined> = this.#declared;

	/** Publish onto `track`, created with {@link INFO}, starting at record zero. */
	constructor(track: Moq.Track.Producer, config?: Config) {
		this.track = track;
		this.#segmenter = new Segmenter(config);
		this.#sink = new Json.Window.Producer<Record>({ track, checkpointRecords: CHECKPOINT_RECORDS });
		this.#publish();
	}

	/** Report a frame of group `group`, presented at `pts`; see {@link Segmenter.frame}. */
	frame(group: number, pts: Micro): void {
		if (this.#done) return;
		try {
			this.#segmenter.frame(group, pts);
		} catch (err) {
			// Records that silently omit frames would misdirect the playlist.
			this.abort(err instanceof Error ? err : new Error(String(err)));
			return;
		}
		this.#publish();
	}

	/** Report that the content extends to `pts`; see {@link Segmenter.end}. */
	end(pts: Micro): void {
		this.#segmenter.end(pts);
	}

	/** Report that content stopped before group `group`; see {@link Segmenter.stop}. */
	stop(group: number): void {
		this.#segmenter.stop(group);
		this.#publish();
	}

	/** Stop after the newest group and finish the record track. Idempotent. */
	finish(): void {
		if (this.#done) return;
		this.#segmenter.close();
		this.#publish();
		this.#done = true;
		this.#sink.finish();
	}

	/** Abort the record track with `err`, so readers see the cause rather than a complete record. Idempotent. */
	abort(err: Error): void {
		if (this.#done) return;
		this.#done = true;
		this.#segmenter.close();
		this.#sink.abort(err);
	}

	#publish(): void {
		if (this.#done) return;
		for (const value of this.#segmenter.take()) {
			try {
				this.#sink.push({ value, at: Moq.Time.Timestamp.now() });
			} catch (err) {
				console.warn("HLS record publish failed; aborting the record track", err);
				this.abort(err instanceof Error ? err : new Error(String(err)));
				return;
			}
		}
		const declared = this.#segmenter.declared;
		if (declared !== undefined && this.#declared.peek() === undefined) {
			this.#declared.set({ track: this.track.name, duration: u53(Math.ceil(declared / 1000)) });
		}
	}
}

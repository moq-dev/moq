/**
 * Publishing the broadcast's timeline: a single track carrying one record per aligned
 * segment, mapping a span of content time to the group ranges that carry it on each media
 * track. A consumer can seek (or build an HLS/DASH playlist) without downloading the media.
 * See the catalog's root {@link Catalog.Timeline} section that advertises it.
 *
 * Facts flow up and policy flows down, meeting in the shared {@link Segmenter}: each media
 * track enrolls with {@link Segmenter.track} and reports every group open through its
 * {@link Recorder}; boundaries come from {@link Segmenter.cut} when the application knows
 * them, or from auto-cut on the driver track's keyframes otherwise. A segment's record is
 * published only once every enrolled track has reported past its end boundary (or closed), so
 * records are self-contained and immediately servable.
 *
 * @module
 */

import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Time } from "@moq/net";
import type * as Catalog from "../catalog";
import { MOQ_EPOCH_UNIX_MILLIS, u53 } from "../catalog";

/**
 * A contiguous run of groups a track contributes to a segment, `start` through `end`
 * inclusive. More than one range in a segment means the group sequence is discontinuous (a
 * gap: groups that never existed).
 */
export interface Range {
	start: number;
	end: number;
	/**
	 * Whether the run's first group starts with a keyframe, i.e. whether a player can join or
	 * switch renditions at this range. Omitted when true (the default).
	 */
	keyframe?: boolean;
}

/**
 * One timeline record: a complete aligned segment. `pts` and `duration` bound its span of
 * content time (in the timeline's timescale), and `tracks` maps each participating media
 * track to the group ranges that carry it. A track absent from `tracks` has no content for
 * the span (a gap).
 */
export interface Record {
	segment: number;
	pts: number;
	duration: number;
	tracks?: { [track: string]: Range[] };
}

/** The default timeline timescale: 1000 units per second (milliseconds). */
export const DEFAULT_TIMESCALE = 1000;

/**
 * The default {@link Segmenter} auto-cut threshold in milliseconds: with nobody cutting
 * explicitly, a new segment starts at the first driver keyframe at least this far past the
 * last boundary.
 */
export const DEFAULT_DURATION_MAX_MS = 2000;

/**
 * The conventional name for a broadcast's timeline track (the `.z` marks the
 * DEFLATE-compressed stream, like the catalog's `.json.z` sibling). The actual name is read
 * from the catalog's root `timeline` section, so this is only a default.
 */
export const DEFAULT_NAME = "timeline.z";

/**
 * What a media track carries, declared when it enrolls via {@link Segmenter.track}. The
 * segmenter prefers a video track as the auto-cut driver, since segment boundaries must land
 * on video keyframes to keep every rendition independently decodable.
 */
export type Kind = "video" | "audio";

/** Options for a {@link Segmenter}. */
export interface SegmenterProps {
	/**
	 * The auto-cut threshold in milliseconds of media time: a new segment starts at the first
	 * driver keyframe at least this far past the last boundary. Irrelevant once
	 * {@link Segmenter.cut} is used (explicit cuts disable auto-cut). Defaults to
	 * {@link DEFAULT_DURATION_MAX_MS}.
	 */
	durationMax?: number;
}

/** One enrolled track's report state. */
interface TrackState {
	kind: Kind;
	// Group opens reported and not yet flushed into a record.
	pending: { sequence: number; pts: Time.Micro; keyframe: boolean }[];
	// The newest reported timestamp: everything earlier is known.
	frontier?: Time.Micro;
	closed: boolean;
}

/**
 * The broadcast's segmenter: the shared boundary list every track's groups map onto.
 *
 * One per broadcast. Media tracks enroll with {@link track} and report group opens through
 * the returned {@link Recorder}; the application (or the auto-cut policy) declares boundaries
 * with {@link cut}. Records flush once complete on every enrolled track, into the attached
 * {@link Producer} (buffered until one attaches).
 */
export class Segmenter {
	#durationMaxUs: number;
	// An explicit cut() arrived: the application owns the boundaries; auto-cut stays off.
	#manual = false;
	// Boundaries of segments not yet flushed: boundaries[0] starts segment #nextSegment.
	#boundaries: Time.Micro[] = [];
	// The newest boundary ever created (never popped), the auto-cut reference point.
	#lastBoundary?: Time.Micro;
	#nextSegment = 0;
	#tracks = new Map<string, TrackState>();
	// The auto-cut driver: the first enrolled video track, else the first enrolled track.
	#driver?: string;
	// Where flushed records go once a Producer attaches; buffered until then.
	#sink?: (record: Record) => void;
	#buffered: Record[] = [];

	constructor(props: SegmenterProps = {}) {
		this.#durationMaxUs = (props.durationMax ?? DEFAULT_DURATION_MAX_MS) * 1000;
	}

	/**
	 * Declare a segment boundary at `pts` (microseconds): the segment before it ends, the one
	 * after starts. For applications that know their boundaries (an imported playlist, on-disk
	 * CMAF segments, an encoder placing keyframes). Cuts must be monotonic; an out-of-order one
	 * is ignored. Cutting ahead of the media is fine: the record still waits for every track's
	 * groups. The first explicit cut turns auto-cut off for good.
	 */
	cut(pts: Time.Micro): void {
		this.#manual = true;
		this.#cutAt(pts);
		this.#tryFlush(false);
	}

	/**
	 * Enroll the media track `name`, returning the {@link Recorder} it reports group opens
	 * through. The segment records key ranges by this name, and the track gates segment
	 * completeness until its recorder closes. Enroll a track when it is about to produce (an
	 * enrolled but silent track holds every record back, by design). One recorder per track:
	 * enrolling the same name again resets its state.
	 */
	track(name: string, kind: Kind): Recorder {
		this.#tracks.set(name, { kind, pending: [], frontier: undefined, closed: false });

		// The first video track drives auto-cut (boundaries must land on its keyframes);
		// without video, the first enrolled track of any kind paces instead.
		const driver = this.#driver ? this.#tracks.get(this.#driver) : undefined;
		if (!driver || (kind === "video" && driver.kind !== "video")) {
			this.#driver = name;
		}

		return new Recorder(this, name);
	}

	#cutAt(pts: Time.Micro): void {
		if (this.#lastBoundary !== undefined && pts <= this.#lastBoundary) return;
		this.#boundaries.push(pts);
		this.#lastBoundary = pts;
	}

	/** @internal A group open reported by a {@link Recorder}. */
	report(name: string, sequence: number, pts: Time.Micro, keyframe: boolean): void {
		const track = this.#tracks.get(name);
		if (!track) return;
		track.pending.push({ sequence, pts, keyframe });
		if (track.frontier === undefined || pts > track.frontier) track.frontier = pts;

		// Auto-cut: only the driver paces, only at a keyframe when the driver is video (an
		// audio driver can cut anywhere), and never once the application cuts explicitly.
		if (!this.#manual && this.#driver === name && (keyframe || track.kind !== "video")) {
			const due = this.#lastBoundary === undefined || pts >= this.#lastBoundary + this.#durationMaxUs;
			if (due) this.#cutAt(pts);
		}

		this.#tryFlush(false);
	}

	/** @internal A track's recorder closed: stop gating completeness on it. */
	close(name: string): void {
		const track = this.#tracks.get(name);
		if (track) track.closed = true;
		if (this.#driver === name) {
			this.#driver = this.#elect();
		}
		this.#tryFlush(false);
	}

	// The auto-cut driver among open tracks: a video track if any, else any open track.
	#elect(): string | undefined {
		let any: string | undefined;
		for (const [name, track] of this.#tracks) {
			if (track.closed) continue;
			if (track.kind === "video") return name;
			any ??= name;
		}
		return any;
	}

	// Flush every segment whose content is final on all tracks. A segment needs its end
	// boundary and, per open track, a report at or past it. `finished` treats every track as
	// closed, for the terminal flush.
	#tryFlush(finished: boolean): void {
		// With nothing enrolled there is nothing to describe; boundaries just wait.
		if (this.#tracks.size === 0) return;

		while (this.#boundaries.length >= 2) {
			const end = this.#boundaries[1];
			if (!finished) {
				for (const track of this.#tracks.values()) {
					if (track.closed) continue;
					if (track.frontier === undefined || track.frontier < end) return;
				}
			}
			const start = this.#boundaries.shift();
			if (start === undefined) return;
			this.#flushSegment(start, end);
		}
	}

	// Emit the record for the segment starting at `start`: drain every track's groups before
	// `end` (all of them for the final, unbounded segment) into ranges. Anything reported
	// before the first boundary lands in the oldest segment.
	#flushSegment(start: Time.Micro, end?: Time.Micro): void {
		const pts = Math.floor((start * DEFAULT_TIMESCALE) / 1_000_000);
		let endUnits: number;
		if (end !== undefined) {
			endUnits = Math.floor((end * DEFAULT_TIMESCALE) / 1_000_000);
		} else {
			// The final segment has no end boundary; its duration runs to the newest report.
			let max = start;
			for (const track of this.#tracks.values()) {
				if (track.frontier !== undefined && track.frontier > max) max = track.frontier;
			}
			endUnits = Math.floor((max * DEFAULT_TIMESCALE) / 1_000_000);
		}

		const record: Record = { segment: this.#nextSegment, pts, duration: Math.max(0, endUnits - pts) };
		this.#nextSegment += 1;

		const tracks: { [track: string]: Range[] } = {};
		let any = false;
		for (const [name, track] of this.#tracks) {
			const ranges: Range[] = [];
			while (track.pending.length > 0) {
				const group = track.pending[0];
				if (end !== undefined && group.pts >= end) break;
				track.pending.shift();
				const last = ranges.at(-1);
				// Contiguous sequences extend the run; a skip starts a new range (a gap:
				// groups that never existed).
				if (last && last.end + 1 === group.sequence) {
					last.end = group.sequence;
				} else {
					const range: Range = { start: group.sequence, end: group.sequence };
					if (!group.keyframe) range.keyframe = false;
					ranges.push(range);
				}
			}
			if (ranges.length > 0) {
				tracks[name] = ranges;
				any = true;
			}
		}
		if (any) record.tracks = tracks;

		this.#emit(record);
	}

	#emit(record: Record): void {
		if (this.#sink) {
			this.#sink(record);
		} else {
			this.#buffered.push(record);
		}
	}

	/** @internal Attach the record sink; records flushed before this are published now. */
	attach(sink: (record: Record) => void): void {
		for (const record of this.#buffered) sink(record);
		this.#buffered.length = 0;
		this.#sink = sink;
	}

	/** @internal The terminal flush: treat every track as closed, then emit the open tail. */
	finish(): void {
		// Content but never a boundary (nobody cut and the driver never reported): anchor the
		// one segment at the earliest report so the content is still indexed.
		if (this.#boundaries.length === 0) {
			let first: Time.Micro | undefined;
			for (const track of this.#tracks.values()) {
				const pts = track.pending[0]?.pts;
				if (pts !== undefined && (first === undefined || pts < first)) first = pts;
			}
			if (first !== undefined) this.#cutAt(first);
		}

		this.#tryFlush(true);

		const start = this.#boundaries.shift();
		if (start === undefined) return;
		// Skip an empty tail: a cut with no content after it describes nothing.
		for (const track of this.#tracks.values()) {
			if (track.pending.length > 0) {
				this.#flushSegment(start, undefined);
				return;
			}
		}
	}
}

/**
 * Reports one media track's group opens into the shared {@link Segmenter}. It is the track's
 * single reporting handle; {@link close} ends the track's enrollment (segments stop waiting
 * on it). Minted by {@link Segmenter.track} and held by a {@link Legacy.Producer} (its
 * `timeline` prop).
 */
export class Recorder {
	#segmenter: Segmenter;
	#name: string;

	/** @internal Minted by {@link Segmenter.track}. */
	constructor(segmenter: Segmenter, name: string) {
		this.#segmenter = segmenter;
		this.#name = name;
	}

	/**
	 * Report that group `sequence` opened at presentation time `pts` (microseconds),
	 * `keyframe` stating whether its first frame is one. Reports must be in group order with
	 * monotonic timestamps.
	 */
	record(sequence: number, pts: Time.Micro, keyframe = true): void {
		this.#segmenter.report(this.#name, sequence, pts, keyframe);
	}

	/** Close the track's enrollment: segments no longer wait on it. */
	close(): void {
		this.#segmenter.close(this.#name);
	}
}

/** Options for a timeline {@link Producer}. */
export interface ProducerProps {
	/** The broadcast's segmenter to publish; defaults to a fresh one. */
	segmenter?: Segmenter;
}

/**
 * Publishes the broadcast's timeline track: one JSON record per complete segment,
 * DEFLATE-compressed (a `@moq/json` stream). Attaches to the {@link Segmenter} as its record
 * sink; advertise it in the catalog's root `timeline` section via {@link section}.
 */
export class Producer {
	#stream: Json.Stream.Producer<Record>;
	#track: string;
	#segmenter: Segmenter;
	// The wall-clock time of pts 0, in timescale units since the moq epoch (advertised in the section).
	#wall?: number;

	/** Wrap an already-created MoQ track (named {@link DEFAULT_NAME} by convention). */
	constructor(track: Moq.Track.Producer, props: ProducerProps = {}) {
		this.#track = track.name;
		this.#segmenter = props.segmenter ?? new Segmenter();
		this.#stream = new Json.Stream.Producer<Record>(track, { compression: true });
		this.#segmenter.attach((record) => this.#stream.append(record));
	}

	/** The segmenter this timeline publishes (enroll tracks and cut boundaries through it). */
	get segmenter(): Segmenter {
		return this.#segmenter;
	}

	/** The catalog's root section advertising this timeline. */
	section(): Catalog.Timeline {
		return {
			track: this.#track,
			timescale: u53(DEFAULT_TIMESCALE),
			wall: this.#wall === undefined ? undefined : u53(this.#wall),
		};
	}

	/**
	 * Set (or replace) the wall-clock anchor advertised in the catalog section, from an observed
	 * pairing of a media timestamp `pts` (microseconds) with its wall-clock time `wall` (defaulting
	 * to now). Stored as the extrapolated wall-clock time of pts 0, the single value the catalog
	 * `wall` field carries: in the timeline's timescale, measured from the moq epoch
	 * ({@link Catalog.MOQ_EPOCH_UNIX_MILLIS}, 2020). Throws if `wall` predates the moq epoch
	 * (unrepresentable).
	 */
	setWall(pts: Time.Micro, wall: Date = new Date()): void {
		const unixMillis = wall.getTime();
		if (unixMillis < MOQ_EPOCH_UNIX_MILLIS) {
			throw new Error(`wall time ${unixMillis} predates the moq epoch ${MOQ_EPOCH_UNIX_MILLIS}`);
		}
		const ptsUnits = Math.floor((pts * DEFAULT_TIMESCALE) / 1_000_000);
		const moqUnits = Math.floor(((unixMillis - MOQ_EPOCH_UNIX_MILLIS) * DEFAULT_TIMESCALE) / 1000);
		this.#wall = Math.max(0, moqUnits - ptsUnits);
	}

	/** Flush the final (still open) segment and finish the track. */
	finish(): void {
		this.#segmenter.finish();
		this.#stream.finish();
	}
}

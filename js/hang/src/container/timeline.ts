/**
 * Publishing the broadcast's timeline: a single track carrying one record per aligned
 * segment, mapping a span of content time to the group ranges that carry it on each media
 * track. A consumer can seek (or build an HLS/DASH playlist) without downloading the media.
 * See the catalog's root {@link Catalog.Timeline} section that advertises it.
 *
 * Facts flow up and policy flows down, meeting in the shared {@link Producer}: each media
 * track enrolls with {@link Producer.track} and reports every group open through its
 * {@link Recorder}. A segment ends at the first group boundary that gives it at least
 * {@link ProducerProps.durationMin} on every enrolled track, unless the application declares
 * its own with {@link Producer.cut}. A segment's record is published only once every enrolled
 * track has reported past its end (or closed), so records are self-contained and immediately
 * servable.
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
 * The conventional {@link ProducerProps.durationMin} (1 second), in milliseconds, for callers
 * with no opinion of their own.
 */
export const DEFAULT_DURATION_MIN_MS = 1000;

/**
 * The conventional name for a broadcast's timeline track (the `.z` marks the
 * DEFLATE-compressed stream, like the catalog's `.json.z` sibling). The actual name is read
 * from the catalog's root `timeline` section, so this is only a default.
 */
export const DEFAULT_NAME = "timeline.z";

/** How a {@link Producer} paces its segments. */
export interface ProducerProps {
	/**
	 * The shortest a segment may be, in milliseconds of media time.
	 *
	 * A segment ends at the first point that is a group boundary on every enrolled track and at
	 * least this far past the segment's start, so the track with the coarsest groups paces the
	 * broadcast: a 2s GOP against a 1s minimum yields 2s segments, and a real-time encoder with
	 * a 10 minute GOP yields one 10 minute segment, because there is nowhere else a segment
	 * could start and stay decodable. A floor rather than a target on purpose: a floor is
	 * always satisfiable (wait longer), while a ceiling is not. Defaults to
	 * {@link DEFAULT_DURATION_MIN_MS}.
	 */
	durationMin?: number;

	/**
	 * The longest a segment may be, in milliseconds of media time, advertised in the catalog
	 * when set.
	 *
	 * Set it only when the publisher can actually promise one, i.e. it controls the encoder's
	 * keyframe cadence. Consumers that need a bound up front use it (an HLS exporter's
	 * EXT-X-TARGETDURATION), so it is a contract rather than a hint: a segment that would exceed
	 * it fails the timeline instead of publishing a record that contradicts the catalog. Leave
	 * it unset when the media decides, which is the common case for real-time and for anything
	 * importing a source the publisher doesn't control.
	 */
	durationMax?: number;

	/**
	 * The wall-clock time of `pts` 0, advertised in the catalog when set.
	 *
	 * It anchors content time to an absolute clock, which is what an HLS
	 * EXT-X-PROGRAM-DATE-TIME or a DASH availabilityStartTime needs: `new Date()` for a live
	 * publisher whose timestamps start now, or the content's real start for a recording.
	 *
	 * Clamped to the moq epoch ({@link Catalog.MOQ_EPOCH_UNIX_MILLIS}, 2020), which the wire
	 * format measures from: an earlier time isn't representable.
	 */
	wall?: Date;
}

/** One enrolled track's report state. */
interface TrackState {
	// Group opens reported and not yet flushed into a record.
	pending: { sequence: number; pts: Time.Micro; keyframe: boolean }[];
	// The newest reported timestamp: everything earlier is known. Advanced by a group open (the
	// group starts there) and by Recorder.end (the content stops there).
	frontier?: Time.Micro;
	closed: boolean;
	// Never paces boundaries or gates completeness, even while open: groups are recorded into
	// whichever segment is open when they arrive. See Producer.passive.
	passive: boolean;
}

/**
 * Publishes the broadcast's timeline track: one JSON record per complete segment,
 * DEFLATE-compressed (a `@moq/json` stream), plus the shared boundary list every media track's
 * groups map onto.
 *
 * One per broadcast. Media tracks enroll with {@link track} and report group opens through the
 * returned {@link Recorder}; an application with its own boundaries overrides the pacing with
 * {@link cut}. Advertise it in the catalog's root `timeline` section via {@link section}.
 */
export class Producer {
	#stream: Json.Stream.Producer<Record>;
	#trackName: string;
	#durationMinUs: number;
	#durationMaxUs?: number;

	// Where the open (unflushed) segment starts; undefined until the first report.
	#start?: Time.Micro;
	// Explicit cut() boundaries not yet reached, in order.
	#cuts: Time.Micro[] = [];
	// A cut() arrived, so the application owns the boundaries from here on and the durationMin
	// pacing stops. Without this the pacing races ahead of a source whose segments are longer
	// than the minimum, closing one before its real boundary is declared.
	#manual = false;
	#nextSegment = 0;
	#tracks = new Map<string, TrackState>();
	// Live reservations: while any exists, no record flushes (more tracks are still enrolling).
	#reservers = 0;
	// A segment overran durationMax, so the timeline stopped publishing.
	#overrun?: Error;
	// The wall-clock time of pts 0, in timescale units since the moq epoch (advertised in the section).
	#wall?: number;

	/** Wrap an already-created MoQ track (named {@link DEFAULT_NAME} by convention). */
	constructor(track: Moq.Track.Producer, props: ProducerProps = {}) {
		this.#trackName = track.name;
		this.#durationMinUs = (props.durationMin ?? DEFAULT_DURATION_MIN_MS) * 1000;
		this.#durationMaxUs = props.durationMax === undefined ? undefined : props.durationMax * 1000;
		if (props.wall !== undefined) {
			const unixMillis = Math.max(props.wall.getTime(), MOQ_EPOCH_UNIX_MILLIS);
			this.#wall = Math.floor(((unixMillis - MOQ_EPOCH_UNIX_MILLIS) * DEFAULT_TIMESCALE) / 1000);
		}
		this.#stream = new Json.Stream.Producer<Record>(track, { compression: true });
	}

	/**
	 * Enroll the media track `name` as a pacing track, returning the {@link Recorder} it reports
	 * through.
	 *
	 * The segment records key ranges by this name, and the track paces boundaries and gates
	 * completeness until its recorder closes. Enroll a track when it is about to produce: an
	 * enrolled but silent track holds every record back, by design, since a segment isn't
	 * complete until every track's content is known. One recorder per track: enrolling the same
	 * name again resets its state.
	 *
	 * This is for tracks whose groups arrive continuously, which is what makes them usable as
	 * boundaries. A track that publishes on its own schedule belongs in {@link passive}.
	 */
	track(name: string): Recorder {
		this.#tracks.set(name, { pending: [], frontier: undefined, closed: false, passive: false });
		return new Recorder(this, name);
	}

	/**
	 * Enroll the track `name` without letting it influence segmentation, returning its
	 * {@link Recorder}.
	 *
	 * A passive track's groups are recorded into whichever segment is open when they arrive, but
	 * it never votes on where a boundary falls and never holds a record back. That is what a track
	 * publishing on its own schedule needs: a catalog that emits a group only when the renditions
	 * change, or an application's metadata track. Enrolling such a track with {@link track}
	 * instead would stall the timeline for good, since a segment cannot end until every pacing
	 * track has reported past its boundary.
	 *
	 * The trade is that a passive track's groups are placed by arrival rather than by content
	 * time: nothing waits for them, so a group that shows up after its segment already flushed is
	 * recorded in the next one. The frames still carry their own timestamps.
	 *
	 * A group that never closes (a `@moq/json` stream log) is recorded once, in the segment its
	 * group opened in, and never again. Roll the group at segment boundaries if its content needs
	 * to be addressable per segment.
	 */
	passive(name: string): Recorder {
		this.#tracks.set(name, { pending: [], frontier: undefined, closed: false, passive: true });
		return new Recorder(this, name);
	}

	/**
	 * Declare a segment boundary at `pts` (microseconds), overriding the
	 * {@link ProducerProps.durationMin} pacing.
	 *
	 * For applications that know their own boundaries (an imported playlist, on-disk CMAF
	 * segments, an encoder placing keyframes). Cutting ahead of the media is fine: the record
	 * still waits for every track's groups. A cut that would make a segment shorter than the
	 * minimum is ignored, so several producers declaring the same boundaries cost nothing.
	 *
	 * The first call takes over for good: {@link ProducerProps.durationMin} pacing stops, since it
	 * would otherwise close a segment just before the caller declares where it really ends.
	 *
	 * Throws if a segment already exceeded {@link ProducerProps.durationMax}.
	 */
	cut(pts: Time.Micro): void {
		if (this.#overrun) throw this.#overrun;

		// Even a cut this rejects says the caller owns the boundaries.
		this.#manual = true;

		const since = this.#cuts.at(-1) ?? this.#start;
		if (since === undefined || pts >= since + this.#durationMinUs) {
			this.#cuts.push(pts);
			this.#pump(false);
		}
	}

	/**
	 * Begin reserving the track set, returning a function that releases the reservation.
	 *
	 * The counterpart to the Rust catalog's `reserve`, for the same reason: while any
	 * reservation is outstanding the track set may still grow, so records are withheld from the
	 * broadcast. A record is immutable once published and its completeness is judged against the
	 * tracks enrolled *at that moment*, so a segment that flushes while a sibling rendition is
	 * still enrolling omits it for good, and that rendition's earlier groups then land in
	 * whatever segment flushes next.
	 *
	 * Take one around whatever brings the tracks up, so a producer that enrolls its renditions
	 * one at a time publishes nothing until they are all in. Reservations nest, and releasing
	 * the same one twice is a no-op; the last one released flushes.
	 */
	reserve(): () => void {
		this.#reservers += 1;
		let released = false;
		return () => {
			if (released) return;
			released = true;
			this.#reservers -= 1;
			this.#pump(false);
		};
	}

	/** The catalog's root section advertising this timeline. */
	section(): Catalog.Timeline {
		const durationMax =
			this.#durationMaxUs === undefined
				? undefined
				: u53(Math.ceil((this.#durationMaxUs * DEFAULT_TIMESCALE) / 1_000_000));
		return {
			track: this.#trackName,
			timescale: u53(DEFAULT_TIMESCALE),
			durationMax,
			wall: this.#wall === undefined ? undefined : u53(this.#wall),
		};
	}

	/**
	 * Flush the final (still open) segment and finish the track.
	 *
	 * Throws if a segment exceeded {@link ProducerProps.durationMax}.
	 */
	finish(): void {
		// Nothing more will enroll, so an outstanding reservation has nothing left to wait for.
		this.#reservers = 0;
		this.#pump(true);

		if (this.#overrun) throw this.#overrun;

		// Skip an empty tail: a boundary with no content after it describes nothing.
		const start = this.#start;
		this.#start = undefined;
		if (start !== undefined) {
			for (const track of this.#tracks.values()) {
				if (track.pending.length > 0) {
					this.#flushSegment(start, undefined);
					break;
				}
			}
		}

		this.#stream.finish();
	}

	/** @internal A group open reported by a {@link Recorder}. */
	report(name: string, sequence: number, pts: Time.Micro, keyframe: boolean): void {
		const track = this.#tracks.get(name);
		if (!track) return;
		track.pending.push({ sequence, pts, keyframe });
		this.#advance(track, pts);
	}

	/**
	 * @internal A track's content ends at `pts`, without a group opening there. This is what
	 * gives the final segment an honest duration, since its end is not a boundary anybody cut.
	 */
	reportEnd(name: string, pts: Time.Micro): void {
		const track = this.#tracks.get(name);
		if (track) this.#advance(track, pts);
	}

	/** @internal A track's recorder closed: stop gating completeness on it. */
	close(name: string): void {
		const track = this.#tracks.get(name);
		if (track) track.closed = true;
		this.#pump(false);
	}

	// Raise a track's frontier, then publish whatever that completed.
	#advance(track: TrackState, pts: Time.Micro): void {
		if (track.frontier === undefined || pts > track.frontier) track.frontier = pts;

		// The first thing a pacing track reports anchors the first segment, so content produced
		// before any boundary exists belongs to the oldest segment rather than to nowhere. A
		// passive track must not anchor it: a catalog published while the encoder is still warming
		// up would otherwise stretch segment 0 across the whole startup gap.
		if (this.#start === undefined && !track.passive) this.#start = pts;

		this.#pump(false);
	}

	// Publish every segment the media has finalized. `finished` is the terminal pass: no track
	// will report again, so one that never reached a boundary stops voting instead of holding
	// the timeline open forever.
	#pump(finished: boolean): void {
		if (this.#tracks.size === 0 || this.#reservers > 0 || this.#overrun) return;
		while (this.#closeSegment(finished)) {
			if (this.#overrun) break;
		}
	}

	// Publish the open segment if the media has finalized it, returning whether it did.
	#closeSegment(finished: boolean): boolean {
		const start = this.#start;
		if (start === undefined) return false;

		// Discard boundaries the timeline has already reached. Only the first is ever consulted,
		// so leaving a spent one there would block every later cut behind it and silently drop
		// the caller back to durationMin pacing.
		while (this.#cuts.length > 0 && this.#cuts[0] <= start) this.#cuts.shift();

		const boundary = this.#boundary(start, finished);
		if (!boundary) return false;
		const [end, cut] = boundary;

		// Every open pacing track has to have reported at or past the boundary, proving its ranges
		// for this segment are final. The track that voted for `end` has by construction; a track
		// with shorter groups can still be behind it. A passive track is excluded: it may publish
		// rarely or never again, so waiting on it would stall the timeline for good.
		if (!finished) {
			for (const track of this.#tracks.values()) {
				if (track.closed || track.passive) continue;
				if (track.frontier === undefined || track.frontier < end) return false;
			}
		}

		this.#flushSegment(start, end);
		if (cut) this.#cuts.shift();
		this.#start = end;
		return true;
	}

	// Where the segment starting at `start` ends: an explicit cut when one is registered,
	// otherwise the first group boundary shared by every track that gives the segment its
	// minimum duration. The boolean reports which.
	#boundary(start: Time.Micro, finished: boolean): [Time.Micro, boolean] | undefined {
		const cut = this.#cuts[0];
		if (cut !== undefined && cut > start) return [cut, true];

		// The application declared a boundary at some point, so it owns them all: pacing here
		// would close a segment the caller is about to cut somewhere else.
		if (this.#manual) return undefined;

		const threshold = start + this.#durationMinUs;

		let end: Time.Micro | undefined;
		for (const track of this.#tracks.values()) {
			if (track.passive) continue;
			const candidate = track.pending.find((group) => group.pts >= threshold)?.pts;
			if (candidate === undefined) {
				// This track has produced nothing past the minimum yet, so ending the segment
				// would strand it. A closed track never will, and neither does anything on the
				// terminal pass, so neither one blocks.
				if (finished || track.closed) continue;
				return undefined;
			}
			// The latest vote wins: it is a group boundary on the coarsest track, and every finer
			// track assigns its groups by start, so no group is split. A closed track still votes:
			// it can't report more, but the groups it did report are boundaries like any other.
			if (end === undefined || candidate > end) end = candidate;
		}

		return end === undefined ? undefined : [end, false];
	}

	// Emit the record for the segment starting at `start`: drain every track's groups before
	// `end` (all of them for the final, unbounded segment) into ranges.
	#flushSegment(start: Time.Micro, end?: Time.Micro): void {
		const pts = Math.floor((start * DEFAULT_TIMESCALE) / 1_000_000);
		let endUnits: number;
		if (end !== undefined) {
			endUnits = Math.floor((end * DEFAULT_TIMESCALE) / 1_000_000);
		} else {
			// The final segment has no end boundary, so it runs to the newest thing any track
			// reported: its end of content when the track reported one, otherwise the last group
			// it opened, which undercounts that group's tail.
			let max = start;
			for (const track of this.#tracks.values()) {
				if (track.frontier !== undefined && track.frontier > max) max = track.frontier;
			}
			endUnits = Math.floor((max * DEFAULT_TIMESCALE) / 1_000_000);
		}
		const duration = Math.max(0, endUnits - pts);

		// The catalog promised a bound and this segment breaks it, so the record would contradict
		// what consumers were told. Fail the timeline rather than publish it: a declared maximum
		// the media can't honor is a bug in the publisher.
		if (this.#durationMaxUs !== undefined) {
			const maxUnits = Math.ceil((this.#durationMaxUs * DEFAULT_TIMESCALE) / 1_000_000);
			if (duration > maxUnits) {
				this.#overrun = new Error(
					`timeline segment ${this.#nextSegment} lasted ${duration}ms, over the declared maximum ${maxUnits}ms`,
				);
				// End the track rather than drop it: the records published before the promise
				// broke are still true, and a consumer that has them should keep them.
				this.#stream.finish();
				return;
			}
		}

		const record: Record = { segment: this.#nextSegment, pts, duration };
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
				// Contiguous sequences extend the run; a skip starts a new range (a gap: groups
				// that never existed).
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

		this.#stream.append(record);
	}
}

/**
 * Reports one media track's group opens into the shared {@link Producer}. It is the track's
 * single reporting handle; {@link close} ends the track's enrollment (segments stop waiting
 * on it). Minted by {@link Producer.track} and held by a {@link Legacy.Producer} (its
 * `timeline` prop).
 */
export class Recorder {
	#timeline: Producer;
	#name: string;

	/** @internal Minted by {@link Producer.track}. */
	constructor(timeline: Producer, name: string) {
		this.#timeline = timeline;
		this.#name = name;
	}

	/**
	 * Report that group `sequence` opened at presentation time `pts` (microseconds),
	 * `keyframe` stating whether its first frame is one. Reports must be in group order with
	 * monotonic timestamps.
	 */
	record(sequence: number, pts: Time.Micro, keyframe = true): void {
		this.#timeline.report(this.#name, sequence, pts, keyframe);
	}

	/**
	 * Report that this track's content extends to `pts` (microseconds), without a group opening
	 * there.
	 *
	 * A group open says where content *starts*; the last group of a broadcast has no successor
	 * to bound it, so its segment would otherwise be published a group short (zero for a segment
	 * that is a single group). Report the end whenever you know it: closing a group, finishing a
	 * track.
	 */
	end(pts: Time.Micro): void {
		this.#timeline.reportEnd(this.#name, pts);
	}

	/** Close the track's enrollment: segments no longer wait on it. */
	close(): void {
		this.#timeline.close(this.#name);
	}
}

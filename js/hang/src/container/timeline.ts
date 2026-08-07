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
 * On the wire the track is a `@moq/json` {@link Json.Window}: a sliding log that appends as
 * segments are indexed and trims as they stop being fetchable, so the timeline describes what is
 * actually available rather than everything ever published.
 *
 * Trimming is driven by the media cache itself, not a timer. The timeline keeps a group handle for
 * every group a segment covers, on every track, and retracts the record once any of them reports
 * the group gone. Holding those handles does not keep the media alive, so the timeline tracks the
 * cache without extending it.
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
	pending: { sequence: number; pts: Time.Micro; keyframe: boolean; group: Moq.Group.Producer }[];
	// The newest reported timestamp: everything earlier is known. Advanced by a group open (the
	// group starts there) and by Recorder.end (the content stops there).
	frontier?: Time.Micro;
	closed: boolean;
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
	#window: Json.Window.Producer<Record>;
	// One span of group handles per record in the window, oldest first. A record stands for the
	// media across its whole segment, so it survives exactly as long as every group it covers.
	#indexed: Moq.Group.Producer[][] = [];
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
		this.#window = new Json.Window.Producer<Record>(track, { compression: true });
	}

	/**
	 * Enroll the media track `name`, returning the {@link Recorder} it reports through.
	 *
	 * The segment records key ranges by this name, and the track paces boundaries and gates
	 * completeness until its recorder closes. Enroll a track when it is about to produce: an
	 * enrolled but silent track holds every record back, by design, since a segment isn't
	 * complete until every track's content is known. One recorder per track: enrolling the same
	 * name again resets its state.
	 */
	track(name: string): Recorder {
		this.#tracks.set(name, { pending: [], frontier: undefined, closed: false });
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

		// Eviction is charged from the frame-write path too, so a group can die after the last
		// record. Without this final sweep the window would close still listing it, and a reader
		// would take the clean end as a complete playlist over media it cannot fetch.
		this.#sweep();

		this.#window.finish();
	}

	/** @internal A group open reported by a {@link Recorder}. */
	report(name: string, group: Moq.Group.Producer, pts: Time.Micro, keyframe: boolean): void {
		const track = this.#tracks.get(name);
		if (!track) return;
		track.pending.push({ sequence: group.sequence, pts, keyframe, group });
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

		// The first thing anybody reports anchors the first segment, so content produced before
		// any boundary exists belongs to the oldest segment rather than to nowhere.
		if (this.#start === undefined) this.#start = pts;

		this.#pump(false);
	}

	// Publish every segment the media has finalized. `finished` is the terminal pass: no track
	// will report again, so one that never reached a boundary stops voting instead of holding
	// the timeline open forever.
	#pump(finished: boolean): void {
		// Sweep first: a retraction and the record that triggers it ride the same window group, and
		// sweeping before appending keeps the window from briefly listing content already gone.
		// Unconditional, since media leaving the cache is worth reporting even while a reservation
		// withholds new records.
		this.#sweep();

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

		// Every open track has to have reported at or past the boundary, proving its ranges for
		// this segment are final. The track that voted for `end` has by construction; a track
		// with shorter groups can still be behind it.
		if (!finished) {
			for (const track of this.#tracks.values()) {
				if (track.closed) continue;
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
				this.#window.finish();
				return;
			}
		}

		const record: Record = { segment: this.#nextSegment, pts, duration };
		this.#nextSegment += 1;

		const tracks: { [track: string]: Range[] } = {};
		let any = false;
		// Every group this segment covers, across every track: the set whose availability the
		// record's own availability is the AND of.
		const covered: Moq.Group.Producer[] = [];
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
				covered.push(group.group);
			}
			if (ranges.length > 0) {
				tracks[name] = ranges;
				any = true;
			}
		}
		if (any) record.tracks = tracks;

		this.#window.append(record);
		this.#indexed.push(covered);
	}

	/**
	 * Retract every record up to and including the newest one that has lost any of its groups.
	 *
	 * A record goes when *any* group it covers is gone, on any track: a consumer fetches the whole
	 * segment, so a hole in the middle breaks it just as thoroughly as a missing head.
	 *
	 * Eviction is not strictly oldest-first: a group refreshed by a fetch is protected while a
	 * newer unread one is evicted in its place. That leaves a hole, and a window has only a head,
	 * so the choice is to advertise the dead record or to drop the live ones in front of it.
	 * Dropping them keeps the promise that everything listed is fetchable, which is the whole point
	 * of the timeline; the cost is a shorter window, and eviction reaches those records shortly
	 * anyway.
	 */
	#sweep(): void {
		let gone = 0;
		this.#indexed.forEach((covered, index) => {
			if (covered.some((group) => group.isGone)) gone = index + 1;
		});
		if (gone === 0) return;

		this.#indexed.splice(0, gone);
		this.#window.trim(gone);
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
	 * Report that `group` opened at presentation time `pts` (microseconds), `keyframe` stating
	 * whether its first frame is one. Reports must be in group order with monotonic timestamps.
	 *
	 * The timeline keeps a read handle on `group` to watch when it leaves the cache, which is what
	 * retracts the segment carrying it. That pins no frames, and the caller keeps ownership.
	 */
	record(group: Moq.Group.Producer, pts: Time.Micro, keyframe = true): void {
		this.#timeline.report(this.#name, group, pts, keyframe);
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

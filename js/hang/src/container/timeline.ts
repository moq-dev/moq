/**
 * Publishing a media track's timeline: a companion track indexing the track's segments (one or
 * more groups each), so a consumer can seek (or build an HLS/DASH playlist) without downloading
 * the media. Segment numbers are aligned across a broadcast's tracks through a shared
 * {@link Segmenter}. See the catalog {@link Catalog.Timeline} section that advertises it.
 *
 * @module
 */

import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Time } from "@moq/net";
import type * as Catalog from "../catalog";
import { MOQ_EPOCH_UNIX_MILLIS, u53 } from "../catalog";

/**
 * One timeline record: segment `segment` of the media track starts at group `group`, at
 * presentation time `pts` (in the timeline's timescale). The segment covers every group up to
 * (excluding) the next record's `group`; segment numbers are aligned across the broadcast's
 * tracks.
 */
export interface Record {
	segment: number;
	group: number;
	pts: number;
}

/** The default timeline timescale: 1000 units per second (milliseconds). */
export const DEFAULT_TIMESCALE = 1000;

/**
 * The default {@link Segmenter} interval: an undriven (audio-only) timeline opens a segment
 * about once per second of media time.
 */
export const DEFAULT_INTERVAL_MS = 1000;

/**
 * The conventional companion timeline track name for a media rendition: `<rendition>.timeline.z`
 * (the `.z` marks the DEFLATE-compressed stream, like the catalog's `.json.z` sibling).
 */
export function trackName(rendition: string): string {
	return `${rendition}.timeline.z`;
}

/**
 * How a track's groups map onto the broadcast's aligned segments.
 *
 * - `"boundary"`: every group opens the next segment (video: groups open on keyframes, and a
 *   segment must start on one, so a video segment is exactly one group). The boundary track
 *   paces the broadcast's segments; wire exactly one per {@link Segmenter}.
 * - `"aligned"`: groups pack into the segments the boundary track opens (audio: many short
 *   groups per segment). The first group at or after each boundary starts the track's slice of
 *   that segment. When no boundary track ever records, the recorder paces segments itself at
 *   the segmenter's interval, so an audio-only broadcast still segments.
 */
export type Cadence = "boundary" | "aligned";

/** Options for a {@link Segmenter}. */
export interface SegmenterProps {
	/**
	 * How often an *undriven* segmenter (no `"boundary"` producer, e.g. an audio-only
	 * broadcast) opens a new segment, in milliseconds of media time. Ignored once a boundary
	 * producer is pacing. Defaults to {@link DEFAULT_INTERVAL_MS}.
	 */
	interval?: number;
}

/**
 * The broadcast-wide segment counter every timeline {@link Producer} shares, so segment numbers
 * align across tracks.
 *
 * Create one per broadcast and pass it to each track's producer. The `"boundary"` producer
 * advances it; `"aligned"` producers read it, falling back to interval pacing only while no
 * boundary producer has spoken.
 */
export class Segmenter {
	// The open segment's number and boundary pts (microseconds), undefined before the first group.
	#current?: { segment: number; boundary: Time.Micro };
	// A "boundary" producer has opened a segment, so aligned producers never self-pace.
	#driven = false;
	// Undriven pacing interval in microseconds.
	#intervalUs: number;

	constructor(props: SegmenterProps = {}) {
		this.#intervalUs = (props.interval ?? DEFAULT_INTERVAL_MS) * 1000;
	}

	/**
	 * A `"boundary"` group open at `pts`: open the next segment and return its number.
	 *
	 * The first boundary adopts a segment an aligned producer self-opened (rather than
	 * stranding it as a number the boundary track never records), re-anchoring its boundary to
	 * `pts`; from then on every call opens a new segment.
	 */
	boundary(pts: Time.Micro): number {
		const segment =
			this.#current === undefined ? 0 : this.#driven ? this.#current.segment + 1 : this.#current.segment;
		this.#current = { segment, boundary: pts };
		this.#driven = true;
		return segment;
	}

	/**
	 * An `"aligned"` group open at `pts`, having last recorded `last`: the segment this group
	 * starts for its track, or undefined if it merely extends the one already recorded.
	 */
	align(pts: Time.Micro, last: number | undefined): number | undefined {
		if (this.#current === undefined) {
			// The very first group of the broadcast: open segment 0 at its timestamp.
			this.#current = { segment: 0, boundary: pts };
			return 0;
		}
		const { segment, boundary } = this.#current;
		if (last !== segment && pts >= boundary) return segment;
		if (!this.#driven && pts >= boundary + this.#intervalUs) {
			this.#current = { segment: segment + 1, boundary: pts };
			return segment + 1;
		}
		return undefined;
	}
}

/** Options for a timeline {@link Producer}. */
export interface ProducerProps {
	/** Units per second for the records' `pts` (and the `wall` anchor). Defaults to milliseconds. */
	timescale?: number;

	/**
	 * The broadcast's shared segment counter, so this track's segment numbers align with its
	 * siblings'. Defaults to a fresh one (a track segmented on its own).
	 */
	segmenter?: Segmenter;

	/**
	 * How this track's groups map onto the aligned segments: `"boundary"` for video (every
	 * group is a segment), `"aligned"` for audio (groups pack into the video's segments).
	 * Defaults to `"boundary"`.
	 */
	cadence?: Cadence;
}

/**
 * Publishes one media track's timeline: an NDJSON record per segment, DEFLATE-compressed.
 *
 * {@link record} maps each group open onto the aligned segment numbering, appending a record
 * when the group starts a segment. Advertise it in the rendition's catalog config via
 * {@link section}, and attach it to a {@link Legacy.Producer} (its `timeline` prop) to record
 * group opens automatically.
 */
export class Producer {
	#stream: Json.Stream.Producer<Record>;
	#track: string;
	#timescale: number;
	// The wall-clock time of pts 0, in timescale units since the moq epoch (advertised in the section).
	#wall?: number;
	#segmenter: Segmenter;
	#cadence: Cadence;
	// The last segment this track recorded; the cursor an aligned producer dedupes against.
	#last?: number;

	/** Wrap an already-created MoQ track (named per {@link trackName}) to publish a rendition's timeline. */
	constructor(track: Moq.Track.Producer, props: ProducerProps = {}) {
		this.#track = track.name;
		this.#timescale = props.timescale ?? DEFAULT_TIMESCALE;
		this.#segmenter = props.segmenter ?? new Segmenter();
		this.#cadence = props.cadence ?? "boundary";
		this.#stream = new Json.Stream.Producer<Record>(track, { compression: true });
	}

	/** The catalog section advertising this timeline, to attach to the rendition's config. */
	section(): Catalog.Timeline {
		return {
			track: this.#track,
			timescale: u53(this.#timescale),
			wall: this.#wall === undefined ? undefined : u53(this.#wall),
		};
	}

	/**
	 * Set (or replace) the wall-clock anchor advertised in the catalog section, from an observed
	 * pairing of a media timestamp `pts` (microseconds) with its wall-clock time `wall` (defaulting
	 * to now). Stored as the extrapolated wall-clock time of pts 0, the single value the catalog
	 * `wall` field carries: in this timeline's timescale, measured from the moq epoch
	 * ({@link Catalog.MOQ_EPOCH_UNIX_MILLIS}, 2020). Throws if `wall` predates the moq epoch
	 * (unrepresentable).
	 */
	setWall(pts: Time.Micro, wall: Date = new Date()): void {
		const unixMillis = wall.getTime();
		if (unixMillis < MOQ_EPOCH_UNIX_MILLIS) {
			throw new Error(`wall time ${unixMillis} predates the moq epoch ${MOQ_EPOCH_UNIX_MILLIS}`);
		}
		const ptsUnits = Math.floor((pts * this.#timescale) / 1_000_000);
		const moqUnits = Math.floor(((unixMillis - MOQ_EPOCH_UNIX_MILLIS) * this.#timescale) / 1000);
		this.#wall = Math.max(0, moqUnits - ptsUnits);
	}

	/**
	 * Record that group `sequence` opened at presentation time `pts` (microseconds). Per the
	 * {@link Cadence}, the group either opens a segment (recorded) or extends the current one
	 * (skipped).
	 */
	record(sequence: number, pts: Time.Micro): void {
		const segment =
			this.#cadence === "boundary" ? this.#segmenter.boundary(pts) : this.#segmenter.align(pts, this.#last);
		if (segment === undefined) return;
		this.#last = segment;
		this.#stream.append({ segment, group: sequence, pts: Math.floor((pts * this.#timescale) / 1_000_000) });
	}

	/** Finish the timeline track. */
	finish(): void {
		this.#stream.finish();
	}
}

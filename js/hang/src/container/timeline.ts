/**
 * Publishing a media track's timeline: a companion track that maps each of the track's groups to
 * its start timestamp, so a consumer can seek (or build an HLS/DASH playlist) without downloading
 * the media. See the catalog {@link Catalog.Timeline} section that advertises it.
 *
 * @module
 */

import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Time } from "@moq/net";
import type * as Catalog from "../catalog";
import { MOQ_EPOCH_UNIX_MILLIS, u53 } from "../catalog";

/** One timeline record: the media track opened `group` at presentation time `pts` (in the timeline's timescale). */
export interface Record {
	group: number;
	pts: number;
}

/** The default timeline timescale: 1000 units per second (milliseconds). */
export const DEFAULT_TIMESCALE = 1000;

/** The default record throttle: at most one record per second of media time. */
export const DEFAULT_GRANULARITY_MS = 1000;

/**
 * The conventional companion timeline track name for a media rendition: `<rendition>.timeline.z`
 * (the `.z` marks the DEFLATE-compressed stream, like the catalog's `.json.z` sibling).
 */
export function trackName(rendition: string): string {
	return `${rendition}.timeline.z`;
}

/** Options for a timeline {@link Producer}. */
export interface ProducerProps {
	/** Units per second for the records' `pts` (and the `wall` anchor). Defaults to milliseconds. */
	timescale?: number;

	/**
	 * Record at most one group per this much media time, in milliseconds. Video keyframes are
	 * already this far apart so every group is indexed; short audio groups are thinned out (a
	 * consumer extrapolates or fetches to fill a gap). Defaults to {@link DEFAULT_GRANULARITY_MS}.
	 */
	granularity?: number;
}

/**
 * Publishes one media track's timeline: an NDJSON record per group open, DEFLATE-compressed.
 *
 * {@link record} appends a group's start once. Advertise it in the rendition's catalog config via
 * {@link section}, and attach it to a {@link Legacy.Producer} (its `timeline` prop) to record group
 * opens automatically.
 */
export class Producer {
	#stream: Json.Stream.Producer<Record>;
	#track: string;
	#timescale: number;
	// The wall-clock time of pts 0, in timescale units since the moq epoch (advertised in the section).
	#wall?: number;
	// Minimum media-time gap between recorded groups (throttle), in microseconds.
	#granularityUs: number;
	// The pts (microseconds) of the last recorded group.
	#lastPts?: number;

	/** Wrap an already-created MoQ track (named per {@link trackName}) to publish a rendition's timeline. */
	constructor(track: Moq.Track, props: ProducerProps = {}) {
		this.#track = track.name;
		this.#timescale = props.timescale ?? DEFAULT_TIMESCALE;
		this.#granularityUs = (props.granularity ?? DEFAULT_GRANULARITY_MS) * 1000;
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
	 * Record that group `sequence` opened at presentation time `pts` (microseconds), unless it
	 * falls within the {@link ProducerProps.granularity} of the last recorded group (skipped, so a
	 * consumer extrapolates or fetches to fill the gap).
	 */
	record(sequence: number, pts: Time.Micro): void {
		if (this.#lastPts !== undefined && pts < this.#lastPts + this.#granularityUs) return;
		this.#lastPts = pts;
		this.#stream.append({ group: sequence, pts: Math.floor((pts * this.#timescale) / 1_000_000) });
	}

	/** Finish the timeline track. */
	finish(): void {
		this.#stream.finish();
	}
}

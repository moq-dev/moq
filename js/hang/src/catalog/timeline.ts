import * as z from "zod/mini";
import { u53, u53Schema } from "./integers";

/**
 * The moq epoch (2020-01-01T00:00:00Z) in Unix-epoch milliseconds.
 *
 * Timeline {@link Timeline.wall} values are measured from here rather than the Unix epoch so the
 * numbers stay small (safely within a 53-bit integer even at fine timescales); a consumer recovers
 * Unix time by adding this back.
 */
export const MOQ_EPOCH_UNIX_MILLIS = 1_577_836_800_000;

/**
 * Describes the broadcast's timeline track: its segment index, one record per aligned segment
 * mapping a span of content time to the group ranges that carry it on each media track, so a
 * consumer can seek (or build an HLS/DASH playlist) without downloading the media itself.
 *
 * Lives at the catalog root: there is one timeline per broadcast, because its whole point is
 * that segments are aligned across the broadcast's tracks. A publisher that doesn't segment
 * simply omits it.
 */
export const TimelineSchema = z.object({
	// The name of the MoQ track carrying the broadcast's segment records.
	track: z.string(),

	// Units per second for the records' `pts` (and `wall`). Defaults to 1000 (milliseconds).
	timescale: z._default(u53Schema, u53(1000)),

	// The declared upper bound on a segment's duration, in `timescale` units. The encoder
	// knows its keyframe cadence (or the cutter its boundaries), so the bound is declared up
	// front rather than observed: a consumer can size buffers, and an HLS exporter can write
	// EXT-X-TARGETDURATION, from the catalog alone. A segment may exceed it only slightly
	// (under half a second, the tolerance HLS's nearest-integer rounding grants), or
	// arbitrarily when the media leaves no valid boundary (a GOP longer than the bound).
	durationMax: u53Schema,

	// The wall-clock time of pts 0, in `timescale` units since the moq epoch
	// ({@link MOQ_EPOCH_UNIX_MILLIS}, 2020-01-01), if known. A consumer derives any group's
	// wall-clock time as `wall + pts`, and Unix time by adding the moq epoch back (for HLS
	// EXT-X-PROGRAM-DATE-TIME / DASH availabilityStartTime). Measured from 2020 rather than 1970 so
	// the value stays small and safely within a 53-bit integer even at fine timescales.
	wall: z.optional(u53Schema),
});

/** A media track's companion timeline description. */
export type Timeline = z.infer<typeof TimelineSchema>;

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
 * Describes a media track's companion timeline track: the track's segment index, mapping each
 * segment (one or more groups) to its starting group and timestamp, so a consumer can seek (or
 * build an HLS/DASH playlist) without downloading the media itself.
 *
 * Present on a {@link VideoConfig} / {@link AudioConfig} when the publisher offers one. It is per
 * media track on purpose: a video segment is a single group while an audio segment packs many
 * shorter ones, so each track needs its own group mapping. Only the segment *numbers* are shared:
 * segment N covers the same span of content time on every track, which is what makes the
 * timelines sufficient for HLS/DASH export.
 */
export const TimelineSchema = z.object({
	// The name of the companion MoQ track carrying this track's segment records.
	track: z.string(),

	// Units per second for the records' `pts` (and `wall`). Defaults to 1000 (milliseconds).
	timescale: z._default(u53Schema, u53(1000)),

	// The wall-clock time of pts 0, in `timescale` units since the moq epoch
	// ({@link MOQ_EPOCH_UNIX_MILLIS}, 2020-01-01), if known. A consumer derives any group's
	// wall-clock time as `wall + pts`, and Unix time by adding the moq epoch back (for HLS
	// EXT-X-PROGRAM-DATE-TIME / DASH availabilityStartTime). Measured from 2020 rather than 1970 so
	// the value stays small and safely within a 53-bit integer even at fine timescales.
	wall: z.optional(u53Schema),
});

/** A media track's companion timeline description. */
export type Timeline = z.infer<typeof TimelineSchema>;

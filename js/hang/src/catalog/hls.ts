import * as z from "@zod/mini";
import { u53Schema } from "./integers";

/**
 * A rendition's HLS segment records, which exist only for HLS compatibility.
 *
 * Opt-in, and only on video and audio renditions. Each rendition numbers its own segments. A
 * rendition without it is not HLS-renderable. See the `Hls` module for the records.
 */
export const HlsSchema = z.object({
	// The track carrying the rendition's segment records, on the catalog's broadcast even when the
	// media lives in a sibling. Conventionally the media track's name plus `.hls`.
	track: z.string(),

	// The nominal segment duration in whole milliseconds, fixed for the life of the track.
	duration: u53Schema,
});

/** A rendition's HLS segment records: the record track and nominal segment duration. */
export type Hls = z.infer<typeof HlsSchema>;

/** An `hls` field that decodes as absent when malformed, so it can never reject the media. */
export const HlsFieldSchema = z.catch(z.optional(HlsSchema), undefined);

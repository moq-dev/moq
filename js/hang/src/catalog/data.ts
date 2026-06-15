import * as z from "zod/mini";

/**
 * A custom, application-defined data track in the catalog.
 *
 * Unlike `audio`/`video`, the catalog says nothing about how to decode a data track. It just
 * advertises that the track exists (keyed by name in {@link DataSchema}) so a consumer can discover
 * and subscribe to it. The optional fields are hints for the consumer, not instructions for hang.
 */
export const DataTrackSchema = z.object({
	/** The MIME type of each frame's payload, e.g. `"application/json"`. Informational. */
	mime: z.optional(z.string()),

	/** A free-form description of the track's contents, for humans. */
	description: z.optional(z.string()),
});

/** A custom, application-defined data track in the catalog. */
export type DataTrack = z.infer<typeof DataTrackSchema>;

/**
 * The `data` catalog section: a map of track name to {@link DataTrack}.
 *
 * Each key is the name of a track within the broadcast (e.g. `"meta.json"`). The tracks are
 * independent of each other, so this is a flat map rather than the rendition groups used by
 * `audio`/`video`.
 */
export const DataSchema = z.record(z.string(), DataTrackSchema);

/** The `data` catalog section: a map of track name to {@link DataTrack}. */
export type Data = z.infer<typeof DataSchema>;

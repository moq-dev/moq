import * as z from "zod/mini";
import { CompressionSchema } from "./compression";
import { ModeSchema } from "./mode";
import { RelativeBroadcastSchema } from "./path";
import { TimelineSchema } from "./timeline";

/**
 * Schema for a single JSON track: application data published as a live JSON document or log.
 *
 * The entry says how to read the track, so a consumer needs the track name and nothing else about
 * the application.
 *
 * A *loose* object: fields this build doesn't recognize pass through untouched, so an entry using
 * a future mode or compression round-trips rather than losing the fields that describe it.
 */
export const JsonConfigSchema = z.looseObject({
	// Optional reference to another broadcast that publishes this track, expressed
	// relative to the broadcast that served this catalog (e.g. "./source").
	// If unset, the track lives in the same broadcast as the catalog.
	broadcast: z.optional(RelativeBroadcastSchema),

	// Whether the track is a latest-value document or an append log. Always stated.
	mode: ModeSchema,

	// The compression applied to each frame, or absent when they are plaintext.
	compression: z.optional(CompressionSchema),

	// An optional identifier for the shape of each value, typically a JSON Schema URL.
	// Purely descriptive: a consumer that doesn't recognize it can still read the track.
	schema: z.optional(z.string()),

	// The companion timeline track indexing this track's groups, if the publisher offers one.
	timeline: z.optional(TimelineSchema),
});

/**
 * Schema for the catalog `json` section: a map of track name to config.
 *
 * Not a rendition set: entries are distinct tracks, not alternatives to choose between. The map
 * key is the track name to subscribe to.
 */
export const JsonSchema = z.object({
	tracks: z.record(z.string(), JsonConfigSchema),
});

/** The catalog JSON section: data tracks keyed by track name. */
export type Json = z.infer<typeof JsonSchema>;
/** How to read one JSON track. */
export type JsonConfig = z.infer<typeof JsonConfigSchema>;

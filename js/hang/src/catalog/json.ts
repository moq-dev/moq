import * as z from "zod/mini";
import { CompressionSchema } from "./compression";
import { u53Schema } from "./integers";
import { ModeSchema } from "./mode";
import { RelativeBroadcastSchema } from "./path";

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

	// The maximum bitrate of the track in bits per second, if known.
	bitrate: z.optional(u53Schema),

	// The maximum delay between a payload being ready and the publisher flushing it, in whole
	// milliseconds rounded up, with the same meaning as a video rendition's `jitter`.
	jitter: z.optional(
		z.pipe(
			u53Schema,
			z.transform((value) => (value === 0 ? undefined : value)),
		),
	),

	// How far this track's payloads reach the transport behind the broadcast's earliest rendition,
	// with the same meaning and encoding as a video rendition's `delay`. Only measured for payloads
	// that carry a capture time.
	delay: z.optional(
		z.pipe(
			u53Schema,
			z.transform((value) => (value === 0 ? undefined : value)),
		),
	),
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

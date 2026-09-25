import * as z from "zod/mini";
import { CompressionSchema } from "./compression";
import { u53Schema } from "./integers";
import { ModeSchema } from "./mode";
import { RelativeBroadcastSchema } from "./path";

/**
 * Schema for a single binary track: application data published as opaque payloads.
 *
 * The entry says how to read the track, so a consumer needs the track name and nothing else about
 * the application. Prefer a {@link JsonConfig} track when the payloads are JSON, so a generic
 * consumer can read them.
 *
 * A *loose* object: fields this build doesn't recognize pass through untouched, so an entry using
 * a future mode or compression round-trips rather than losing the fields that describe it.
 */
export const BinaryConfigSchema = z.looseObject({
	// Optional reference to another broadcast that publishes this track, expressed
	// relative to the broadcast that served this catalog (e.g. "./source").
	// If unset, the track lives in the same broadcast as the catalog.
	broadcast: z.optional(RelativeBroadcastSchema),

	// Whether the track is a latest-value blob or an append log. Always stated.
	mode: ModeSchema,

	// The compression applied to each frame, or absent when they are written through untouched.
	compression: z.optional(CompressionSchema),

	// An optional media type for each payload (e.g. "image/jpeg"). Purely descriptive:
	// a consumer that doesn't recognize it can still read the track.
	mime: z.optional(z.string()),

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
});

/**
 * Schema for the catalog `binary` section: a map of track name to config.
 *
 * Not a rendition set: entries are distinct tracks, not alternatives to choose between. The map
 * key is the track name to subscribe to.
 */
export const BinarySchema = z.object({
	tracks: z.record(z.string(), BinaryConfigSchema),
});

/** The catalog binary section: data tracks keyed by track name. */
export type Binary = z.infer<typeof BinarySchema>;
/** How to read one binary track. */
export type BinaryConfig = z.infer<typeof BinaryConfigSchema>;

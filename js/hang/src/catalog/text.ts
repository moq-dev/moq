import * as z from "zod/mini";
import { ContainerSchema } from "./container";
import { u53Schema } from "./integers";
import { RelativeBroadcastSchema } from "./path";
import { TimelineSchema } from "./timeline";

/**
 * The cue serialization format of a text track, mirroring the Rust `TextFormat`.
 *
 * Known values are `"vtt"` (WebVTT), `"ttml"` (TTML/IMSC1), and `"utf8"` (raw text). Any other
 * string is an unrecognized format the consumer should ignore. The frame timestamp always carries
 * the cue's start time, so a relay never parses the payload.
 */
export const TextFormatSchema = z.string();

/**
 * The accessibility role of a text track, mirroring the Rust `TextRole`.
 *
 * Known values are `"subtitle"` (dialogue only) and `"caption"` (all audio, i.e. SDH). It is a
 * plain string rather than an enum because the vocabulary is borrowed from
 * [draft-ietf-moq-msf](https://datatracker.ietf.org/doc/draft-ietf-moq-msf/) and is expected to
 * grow: an unrecognized role must leave the rendition listed (and republishable verbatim), not
 * drop the catalog's captions. Treat anything else as `"subtitle"`.
 */
export const TextRoleSchema = z.string();

/**
 * Schema for a single text (caption/subtitle) rendition.
 *
 * Unlike audio and video there is no WebCodecs decoder for text: the consumer parses each cue's
 * payload directly (per {@link TextFormatSchema}) and renders it as an overlay.
 */
export const TextConfigSchema = z.object({
	// Optional reference to another broadcast that publishes this track, expressed relative to the
	// broadcast that served this catalog (e.g. "../source"). If unset, the track lives in the same
	// broadcast as the catalog.
	broadcast: z.optional(RelativeBroadcastSchema),

	// The cue serialization format: "vtt" | "ttml" | "utf8" | (unknown).
	format: TextFormatSchema,

	// The accessibility role, defaulting to "subtitle".
	role: z._default(TextRoleSchema, "subtitle"),

	// The BCP-47 language tag (e.g. "en", "es-419"), if known.
	lang: z.optional(z.string()),

	// A human-readable label for a track picker, when `lang` alone is ambiguous.
	label: z.optional(z.string()),

	// The container format, used to decode the timestamp and frame the payload.
	container: ContainerSchema,

	// The maximum jitter before the next cue is flushed, in milliseconds. The player's jitter buffer
	// should be at least this large; absent means each cue is flushed immediately.
	jitter: z.optional(u53Schema),

	// The companion timeline track indexing this rendition's groups, if the publisher offers one.
	timeline: z.optional(TimelineSchema),
});

/** Schema for the catalog text section: a map of track name to rendition config. */
export const TextSchema = z.object({
	// A map of track name to rendition configuration.
	// This is not an array so it will work with JSON Merge Patch.
	renditions: z.record(z.string(), TextConfigSchema),
});

/** The catalog text section: renditions keyed by track name. */
export type Text = z.infer<typeof TextSchema>;
/** Config for a single text rendition. */
export type TextConfig = z.infer<typeof TextConfigSchema>;
/** The accessibility role of a text track. */
export type TextRole = z.infer<typeof TextRoleSchema>;

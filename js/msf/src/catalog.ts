import type * as Moq from "@moq/net";
import * as z from "zod/mini";

/** Zod schema for a track's wire packaging. Accepts known values or any future string. */
export const PackagingSchema = z.union([
	z.enum(["loc", "cmaf", "legacy", "mediatimeline", "eventtimeline"]),
	z.string(),
]);

/** How a track's frames are packaged on the wire (e.g. "loc" or "cmaf"). */
export type Packaging = z.infer<typeof PackagingSchema>;

/** Zod schema for a track's role. Accepts known values or any future string. */
export const RoleSchema = z.union([
	z.enum(["video", "audio", "audiodescription", "caption", "subtitle", "signlanguage"]),
	z.string(),
]);

/** The semantic role a track plays in the presentation (e.g. "video", "audio", "caption"). */
export type Role = z.infer<typeof RoleSchema>;

/** Zod schema describing a single track entry in an MSF catalog. */
export const TrackSchema = z.object({
	name: z.string(),
	packaging: PackagingSchema,
	isLive: z.boolean(),
	role: z.optional(RoleSchema),
	codec: z.optional(z.string()),
	width: z.optional(z.number()),
	height: z.optional(z.number()),
	framerate: z.optional(z.number()),
	samplerate: z.optional(z.number()),
	channelConfig: z.optional(z.string()),
	bitrate: z.optional(z.number()),
	initData: z.optional(z.string()),
	renderGroup: z.optional(z.number()),
	altGroup: z.optional(z.number()),

	// Non-standard: maximum delay (ms) between consecutive frames on this track.
	// The player's buffer must be at least this large to avoid underruns.
	// Mirrors the `jitter` field in the hang catalog.
	jitter: z.optional(z.number()),
});

/** A single track in an MSF catalog, including its codec and media properties. */
export type Track = z.infer<typeof TrackSchema>;

/** Zod schema for the top-level MSF catalog (version 1). */
export const CatalogSchema = z.object({
	version: z.literal(1),
	tracks: z.array(TrackSchema),
});

/** The MSF catalog: a versioned list of available tracks. */
export type Catalog = z.infer<typeof CatalogSchema>;

/** Serialize a catalog to its JSON wire representation. */
export function encode(catalog: Catalog): Uint8Array {
	const encoder = new TextEncoder();
	return encoder.encode(JSON.stringify(catalog));
}

/** Parse and validate a catalog from its JSON wire representation. Throws if invalid. */
export function decode(raw: Uint8Array): Catalog {
	const decoder = new TextDecoder();
	const str = decoder.decode(raw);
	try {
		const json = JSON.parse(str);
		return CatalogSchema.parse(json);
	} catch (error) {
		console.warn("invalid MSF catalog", str);
		throw error;
	}
}

/** Read and decode the next catalog frame from a track, or undefined if the track ended. */
export async function fetch(track: Moq.Track): Promise<Catalog | undefined> {
	const frame = await track.readFrame();
	if (!frame) return undefined;
	return decode(frame);
}

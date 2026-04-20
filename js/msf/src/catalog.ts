import type * as Moq from "@moq/lite";
import * as z from "zod/mini";

export const PackagingSchema = z.union([
	z.enum(["loc", "cmaf", "legacy", "mediatimeline", "eventtimeline"]),
	z.string(),
]);

export type Packaging = z.infer<typeof PackagingSchema>;

export const RoleSchema = z.union([
	z.enum(["video", "audio", "audiodescription", "caption", "subtitle", "signlanguage"]),
	z.string(),
]);

export type Role = z.infer<typeof RoleSchema>;

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

export type Track = z.infer<typeof TrackSchema>;

export const CatalogSchema = z.object({
	version: z.literal(1),
	tracks: z.array(TrackSchema),
});

export type Catalog = z.infer<typeof CatalogSchema>;

export function encode(catalog: Catalog): Uint8Array {
	const encoder = new TextEncoder();
	return encoder.encode(JSON.stringify(catalog));
}

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

export async function fetch(track: Moq.Track): Promise<Catalog | undefined> {
	const frame = await track.readFrame();
	if (!frame) return undefined;
	return decode(frame);
}

import type * as Moq from "@moq/lite";
import { z } from "zod";

export const PackagingSchema = z.enum(["loc", "cmaf", "legacy", "mediatimeline", "eventtimeline"]).or(z.string());

export type Packaging = z.infer<typeof PackagingSchema>;

export const RoleSchema = z
	.enum(["video", "audio", "audiodescription", "caption", "subtitle", "signlanguage"])
	.or(z.string());

export type Role = z.infer<typeof RoleSchema>;

export const TrackSchema = z.object({
	name: z.string(),
	packaging: PackagingSchema,
	isLive: z.boolean(),
	role: RoleSchema.optional(),
	codec: z.string().optional(),
	width: z.number().optional(),
	height: z.number().optional(),
	framerate: z.number().optional(),
	samplerate: z.number().optional(),
	channelConfig: z.string().optional(),
	bitrate: z.number().optional(),
	initData: z.string().optional(),
	renderGroup: z.number().optional(),
	altGroup: z.number().optional(),
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

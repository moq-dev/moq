import * as z from "zod/mini";
import { ContainerSchema } from "./container";
import { u53Schema } from "./integers";

// Configuration for a single thumbnail rendition.
// Thumbnails are stand-alone still images (e.g. JPEG/PNG/WebP) published
// at most once every `interval` seconds, used to populate a paused player
// without having to subscribe to the full video track for an i-frame.
export const ThumbnailConfigSchema = z.object({
	// MIME type of the encoded image, e.g. "image/jpeg", "image/png", "image/webp".
	codec: z.string(),

	// The container format, used to decode the timestamp and more.
	container: ContainerSchema,

	// The dimensions of the encoded image in pixels.
	codedWidth: u53Schema,
	codedHeight: u53Schema,

	// The minimum interval between thumbnails in milliseconds.
	// Subscribers can use this as a hint for how often to poll.
	interval: z.optional(u53Schema),

	// JPEG/WebP quality the publisher targeted (0-1). Informational.
	quality: z.optional(z.number()),
});

export const ThumbnailSchema = z.object({
	// A map of track name to rendition configuration.
	// Multiple renditions allow subscribers to pick a size close to their canvas.
	renditions: z.record(z.string(), ThumbnailConfigSchema),
});

export type Thumbnail = z.infer<typeof ThumbnailSchema>;
export type ThumbnailConfig = z.infer<typeof ThumbnailConfigSchema>;

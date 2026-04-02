import * as z from "zod/mini";
import { ContainerSchema } from "./container";
import { u53Schema } from "./integers";

// Backwards compatibility: old track schema
const TrackSchema = z.object({
	name: z.string(),
});

// Based on VideoDecoderConfig
export const VideoConfigSchema = z.object({
	// See: https://w3c.github.io/webcodecs/codec_registry.html
	codec: z.string(),

	// The container format, used to decode the timestamp and more.
	container: ContainerSchema,

	// The description is used for some codecs.
	// If provided, we can initialize the decoder based on the catalog alone.
	// Otherwise, the initialization information is (repeated) before each key-frame.
	description: z.optional(z.string()), // hex encoded TODO use base64

	// The width and height of the video in pixels.
	// NOTE: formats that don't use a description can adjust these values in-band.
	codedWidth: z.optional(u53Schema),
	codedHeight: z.optional(u53Schema),

	// Ratio of display width/height to coded width/height
	// Allows stretching/squishing individual "pixels" of the video
	// If not provided, the display ratio is 1:1
	displayAspectWidth: z.optional(u53Schema),
	displayAspectHeight: z.optional(u53Schema),

	// The frame rate of the video in frames per second
	framerate: z.optional(z.number()),

	// The bitrate of the video in bits per second
	// TODO: Support up to Number.MAX_SAFE_INTEGER
	bitrate: z.optional(u53Schema),

	// If true, the decoder will optimize for latency.
	// Default: true
	optimizeForLatency: z.optional(z.boolean()),

	// The maximum jitter before the next frame is emitted in milliseconds.
	// The player's jitter buffer should be larger than this value.
	// If not provided, the player should assume each frame is flushed immediately.
	//
	// ex:
	// - If each frame is flushed immediately, this would be 1000/fps.
	// - If there can be up to 3 b-frames in a row, this would be 3 * 1000/fps.
	// - If frames are buffered into 2s segments, this would be 2s.
	jitter: z.optional(u53Schema),
});

// Mirrors VideoDecoderConfig
// https://w3c.github.io/webcodecs/#video-decoder-config
export const VideoSchema = z.union([
	z.object({
		// A map of track name to rendition configuration.
		// This is not an array in order for it to work with JSON Merge Patch.
		renditions: z.record(z.string(), VideoConfigSchema),

		// Render the video at this size in pixels.
		// This is separate from the display aspect ratio because it does not require reinitialization.
		display: z.optional(
			z.object({
				width: u53Schema,
				height: u53Schema,
			}),
		),

		// The rotation of the video in degrees.
		// Default: 0
		rotation: z.optional(z.number()),

		// If true, the decoder will flip the video horizontally
		// Default: false
		flip: z.optional(z.boolean()),
	}),
	// Backwards compatibility: transform old array of {track, config} to new object format
	z.pipe(
		z.array(
			z.object({
				track: TrackSchema,
				config: VideoConfigSchema,
			}),
		),
		z.transform((arr) => {
			const config = arr[0]?.config;
			return {
				renditions: Object.fromEntries(arr.map((item) => [item.track.name, item.config])),
				display:
					config?.displayAspectWidth !== undefined && config?.displayAspectHeight !== undefined
						? { width: config.displayAspectWidth, height: config.displayAspectHeight }
						: undefined,
				rotation: undefined,
				flip: undefined,
			};
		}),
	),
]);

export type Video = z.infer<typeof VideoSchema>;
export type VideoConfig = z.infer<typeof VideoConfigSchema>;

import { z } from "zod";
import { ContainerSchema } from "./container";
import { u53Schema } from "./integers";

// Backwards compatibility: old track schema
const TrackSchema = z.object({
	name: z.string(),
	priority: z.number().int().min(0).max(255),
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
	description: z.string().optional(), // hex encoded TODO use base64

	// The width and height of the video in pixels.
	// NOTE: formats that don't use a description can adjust these values in-band.
	codedWidth: u53Schema.optional(),
	codedHeight: u53Schema.optional(),

	// Ratio of display width/height to coded width/height
	// Allows stretching/squishing individual "pixels" of the video
	// If not provided, the display ratio is 1:1
	displayAspectWidth: u53Schema.optional(),
	displayAspectHeight: u53Schema.optional(),

	// The frame rate of the video in frames per second
	framerate: z.number().optional(),

	// The bitrate of the video in bits per second
	// TODO: Support up to Number.MAX_SAFE_INTEGER
	bitrate: u53Schema.optional(),

	// If true, the decoder will optimize for latency.
	// Default: true
	optimizeForLatency: z.boolean().optional(),

	// The maximum jitter before the next frame is emitted in milliseconds.
	// The player's jitter buffer should be larger than this value.
	// If not provided, the player should assume each frame is flushed immediately.
	//
	// ex:
	// - If each frame is flushed immediately, this would be 1000/fps.
	// - If there can be up to 3 b-frames in a row, this would be 3 * 1000/fps.
	// - If frames are buffered into 2s segments, this would be 2s.
	jitter: u53Schema.optional(),
});

// Mirrors VideoDecoderConfig
// https://w3c.github.io/webcodecs/#video-decoder-config
export const VideoSchema = z
	.object({
		// A map of track name to rendition configuration.
		// This is not an array in order for it to work with JSON Merge Patch.
		renditions: z.record(z.string(), VideoConfigSchema),

		// The priority of the video track, relative to other tracks in the broadcast.
		priority: z.number().int().min(0).max(255),

		// Render the video at this size in pixels.
		// This is separate from the display aspect ratio because it does not require reinitialization.
		display: z
			.object({
				width: u53Schema,
				height: u53Schema,
			})
			.optional(),

		// The rotation of the video in degrees.
		// Default: 0
		rotation: z.number().optional(),

		// If true, the decoder will flip the video horizontally
		// Default: false
		flip: z.boolean().optional(),
	})
	.or(
		// Backwards compatibility: transform old array of {track, config} to new object format
		z
			.array(
				z.object({
					track: TrackSchema,
					config: VideoConfigSchema,
				}),
			)
			.transform((arr) => {
				const config = arr[0]?.config;
				return {
					renditions: Object.fromEntries(arr.map((item) => [item.track.name, item.config])),
					priority: arr[0]?.track.priority ?? 128,
					display:
						config?.displayAspectWidth && config?.displayAspectHeight
							? { width: config.displayAspectWidth, height: config.displayAspectHeight }
							: undefined,
					rotation: undefined,
					flip: undefined,
				};
			}),
	);

export type Video = z.infer<typeof VideoSchema>;
export type VideoConfig = z.infer<typeof VideoConfigSchema>;

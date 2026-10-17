import * as z from "@zod/mini";
import { compareName, compareNumber } from "./compare";
import { ContainerSchema } from "./container";
import { hexSchema } from "./hex";
import { u53Schema } from "./integers";
import { RelativeBroadcastSchema } from "./path";

// Backwards compatibility: old track schema
const TrackSchema = z.object({
	name: z.string(),
});

/** Schema for a single video rendition's decoder config. Mirrors WebCodecs VideoDecoderConfig. */
export const VideoConfigSchema = z.object({
	// Optional reference to another broadcast that publishes this track, expressed
	// relative to the broadcast that served this catalog (e.g. "./source").
	// If unset, the track lives in the same broadcast as the catalog.
	broadcast: z.optional(RelativeBroadcastSchema),
	// The track's name in its broadcast, when it differs from this entry's ID (its key in the
	// catalog). Absent means the ID is the track name. Resolve it with `track`.
	track: z.optional(z.string()),

	// Human-readable rendition name for track pickers.
	label: z.optional(z.string()),

	// See: https://w3c.github.io/webcodecs/codec_registry.html
	codec: z.string(),

	// The container format, used to decode the timestamp and more.
	container: ContainerSchema,

	// The description is used for some codecs.
	// If provided, we can initialize the decoder based on the catalog alone.
	// Otherwise, the initialization information is (repeated) before each key-frame.
	description: z.optional(hexSchema),

	// The largest width and height the track carries in pixels, fixed for its name. The picture
	// resizes in band at a keyframe below these ceilings, unless the description pins it.
	codedWidth: z.optional(u53Schema),
	codedHeight: z.optional(u53Schema),

	// The aspect ratio the whole picture is displayed at, as WebCodecs' VideoDecoderConfig
	// displayAspectWidth/displayAspectHeight. Absent means the decoded picture's own shape.
	displayAspectWidth: z.optional(u53Schema),
	displayAspectHeight: z.optional(u53Schema),

	// The maximum frame rate of the video in frames per second.
	framerate: z.optional(z.number()),

	// The maximum bitrate of the video in bits per second.
	// TODO: Support up to Number.MAX_SAFE_INTEGER
	bitrate: z.optional(u53Schema),

	// Whether this rendition may be selected. When false, no frames are coming and a consumer
	// must not select it. Default: true, so publishers only write it when false.
	enabled: z.optional(z.boolean()),

	// Higher is preferred: a player selects among the highest preference it can decode, then by
	// picture and bitrate within it. A compatibility transcode is -1 so only a player that can't
	// decode the source pulls it. Default: 0, so publishers only write it when nonzero.
	preference: z.optional(z.int32()),

	// If true, the decoder will optimize for latency.
	// Default: true
	optimizeForLatency: z.optional(z.boolean()),

	// The maximum delay between a frame being ready and the publisher flushing it, in whole
	// milliseconds rounded up. The player's jitter buffer should be larger than this value.
	// If not provided, the player should assume each frame is flushed immediately.
	//
	// This is measured at the publisher (encoder latency, segment size, B-frame reordering),
	// never on the network a consumer sees. It only ever grows over the life of a stream.
	//
	// ex:
	// - If each frame is flushed immediately, this would be 1000/fps.
	// - If there can be up to 3 b-frames in a row, this would be 3 * 1000/fps.
	// - If frames are buffered into 2s segments, this would be 2s.
	jitter: z.optional(
		z.pipe(
			u53Schema,
			z.transform((value) => (value === 0 ? undefined : value)),
		),
	),

	// After a non-continuous join, decode from the group start, present at start plus warmup,
	// and join that many milliseconds earlier.
	warmup: z.optional(u53Schema),

	// How far this rendition's frames reach the transport behind the broadcast's earliest
	// rendition, in whole milliseconds rounded up. A player holds `delay + jitter` for it and never
	// subtracts one rendition's `delay` from another's. Absent on the earliest rendition. It only
	// ever grows over the life of a stream.
	delay: z.optional(
		z.pipe(
			u53Schema,
			z.transform((value) => (value === 0 ? undefined : value)),
		),
	),
});

/**
 * Schema for the catalog video section: renditions plus display size, rotation, and flip.
 * Renditions mirror WebCodecs VideoDecoderConfig (https://w3c.github.io/webcodecs/#video-decoder-config).
 */
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

/** The catalog video section: renditions keyed by track name plus display options. */
export type Video = z.infer<typeof VideoSchema>;
/** Decoder config for a single video rendition. */
export type VideoConfig = z.infer<typeof VideoConfigSchema>;

/** Rank video best first: enabled, then highest preference, largest picture, highest bitrate, and name. */
export function ranked(renditions: Record<string, VideoConfig>): [string, VideoConfig][] {
	return Object.entries(renditions).sort(compareRanked);
}

/** Coded pixel count. A missing side is zero, so an unknown picture ranks below a known one. */
function pictureArea(config: VideoConfig): number {
	return (config.codedWidth ?? 0) * (config.codedHeight ?? 0);
}

function compareRanked(left: [string, VideoConfig], right: [string, VideoConfig]): number {
	return (
		compareNumber(Number(left[1].enabled !== false), Number(right[1].enabled !== false)) ||
		compareNumber(left[1].preference ?? 0, right[1].preference ?? 0) ||
		compareNumber(pictureArea(left[1]), pictureArea(right[1])) ||
		compareNumber(left[1].bitrate, right[1].bitrate) ||
		compareName(left[0], right[0])
	);
}

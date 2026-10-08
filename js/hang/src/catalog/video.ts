import * as z from "@zod/mini";
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

	// Whether the publisher recommends temporarily avoiding this rendition.
	// The track remains available and may still be selected as a fallback.
	stalled: z.optional(z.boolean()),

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

/**
 * Rank video renditions best first: largest picture, then highest bitrate, then name.
 *
 * A missing width, height, or bitrate ranks below a known value. Exact ties keep name order,
 * matching `hang::catalog::Video::ranked`, so the choice does not follow catalog insertion order.
 */
export function ranked(renditions: Record<string, VideoConfig>): [string, VideoConfig][] {
	return Object.entries(renditions).sort(compareRanked);
}

/** Coded pixel count. A missing side is zero, so an unknown picture ranks below a known one. */
function pictureArea(config: VideoConfig): number {
	return (config.codedWidth ?? 0) * (config.codedHeight ?? 0);
}

function compareRanked(left: [string, VideoConfig], right: [string, VideoConfig]): number {
	const leftArea = pictureArea(left[1]);
	const rightArea = pictureArea(right[1]);
	if (leftArea !== rightArea) return leftArea > rightArea ? -1 : 1;

	const bitrate = compareBitrate(left[1].bitrate, right[1].bitrate);
	if (bitrate !== 0) return bitrate;

	return compareName(left[0], right[0]);
}

// A missing bitrate ranks below every known value, including zero. Higher known bitrates come first.
function compareBitrate(left: number | undefined, right: number | undefined): number {
	if (left === right) return 0;
	if (left === undefined) return 1;
	if (right === undefined) return -1;
	return left > right ? -1 : 1;
}

// Rust `String` order is Unicode code point order. JS `<` is UTF-16 code unit order and disagrees
// once a name leaves the BMP.
function compareName(left: string, right: string): number {
	const l = left[Symbol.iterator]();
	const r = right[Symbol.iterator]();
	for (;;) {
		const a = l.next();
		const b = r.next();
		if (a.done && b.done) return 0;
		if (a.done) return -1;
		if (b.done) return 1;
		const ac = a.value.codePointAt(0) ?? 0;
		const bc = b.value.codePointAt(0) ?? 0;
		if (ac !== bc) return ac < bc ? -1 : 1;
	}
}

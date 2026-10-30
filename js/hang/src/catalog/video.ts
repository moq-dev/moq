import * as z from "@zod/mini";
import { compareName, compareNumber } from "./compare";
import { ContainerSchema } from "./container";
import { hexSchema } from "./hex";
import { HlsFieldSchema } from "./hls";
import { u8Schema, u53Schema } from "./integers";
import { EpochSchema, RelativeBroadcastSchema } from "./path";

// Backwards compatibility: old track schema
const TrackSchema = z.object({
	name: z.string(),
});

// A CIE 1931 xy chromaticity coordinate.
const ChromaticitySchema = z.object({ x: z.number(), y: z.number() });

// A CTA-861.3 light level in candelas per square metre; zero means unknown.
const lightSchema = z.number().check(z.int(), z.nonnegative(), z.lte(0xffff));

/**
 * Schema for the colour a rendition's pixels carry. The code points are ITU-T H.273's, which every
 * codec and container shares; an absent field was left unspecified by the source.
 */
export const ColorSchema = z.object({
	// H.273 ColourPrimaries: 1 is BT.709, 9 is BT.2020.
	primaries: z.optional(u8Schema),
	// H.273 TransferCharacteristics: 1 is BT.709, 16 is PQ, 18 is HLG.
	transfer: z.optional(u8Schema),
	// H.273 MatrixCoefficients: 0 is RGB, 1 is BT.709, 9 is BT.2020 non-constant luminance.
	matrix: z.optional(u8Schema),
	// The numeric range of the samples: studio swing, or every code used.
	range: z.optional(z.enum(["limited", "full"])),
	// The display the content was mastered on, as SMPTE ST 2086, luminance in cd/m².
	mastering: z.optional(
		z.object({
			red: ChromaticitySchema,
			green: ChromaticitySchema,
			blue: ChromaticitySchema,
			white: ChromaticitySchema,
			maxLuminance: z.number(),
			minLuminance: z.number(),
		}),
	),
	// The content's light levels, as CTA-861.3.
	contentLight: z.optional(z.object({ maxCll: lightSchema, maxFall: lightSchema })),
});

/** The colour a rendition's pixels carry: H.273 code points plus static HDR metadata. */
export type Color = z.infer<typeof ColorSchema>;

/** The projection kinds this build knows. */
const KNOWN_PROJECTIONS = ["rectilinear", "equirectangular"];

/**
 * Schema for how a rendition's picture maps onto the viewer's field of view. Absent means rectilinear.
 *
 * An unrecognized kind parses as `{ kind: "unknown", raw }` instead of throwing, and a consumer treats
 * it as absent. A recognized kind only parses through its own schema, so a malformed one throws.
 */
export const ProjectionSchema = z.union([
	z.discriminatedUnion("kind", [
		// A flat picture.
		z.object({ kind: z.literal("rectilinear") }),
		// Longitude linear across the width and latitude down the height, the full 180 degrees,
		// surrounding a viewer at its centre.
		z.object({
			kind: z.literal("equirectangular"),
			// The degrees of longitude the picture spans, centred straight ahead. Absent means 360; VR180 is 180.
			fov: z.optional(z.number().check(z.positive(), z.lte(360))),
		}),
	]),
	z.pipe(
		z.looseObject({
			kind: z.string().check(
				z.refine((kind) => !KNOWN_PROJECTIONS.includes(kind), {
					message: "recognized projection kind must match its own schema",
				}),
			),
		}),
		z.transform((raw) => ({ kind: "unknown" as const, raw })),
	),
]);

/** How a rendition's picture maps onto the viewer's field of view. */
export type Projection = z.infer<typeof ProjectionSchema>;

/** Schema for a single video rendition's decoder config. Mirrors WebCodecs VideoDecoderConfig. */
export const VideoConfigSchema = z.object({
	// Optional reference to another broadcast that publishes this track, expressed
	// relative to the broadcast that served this catalog (e.g. "./source").
	// If unset, the track lives in the same broadcast as the catalog.
	broadcast: z.optional(RelativeBroadcastSchema),
	// The publisher instance the reference names, when known. Absent accepts whichever instance
	// resolves first. An epoch-aware route proving another instance fails the entry; an epochless
	// route cannot prove one, so there it is best effort. Without `broadcast`, it names the
	// catalog's own instance.
	epoch: z.optional(EpochSchema),
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

	// The colour the decoded pixels carry, so a renderer need not guess or reparse the bitstream.
	color: z.optional(ColorSchema),

	// How the picture maps onto the viewer's field of view. Absent means rectilinear.
	projection: z.optional(ProjectionSchema),

	// How each eye's view is packed into the frame: "mono", "left-right" (left eye in the left
	// half), or "top-bottom" (left eye in the top half). Absent means mono. Typed as a plain string
	// so an unrecognized packing survives a republish; a consumer treats one as absent.
	stereo: z.optional(z.string()),

	// Each picture weaves two fields, so a renderer deinterlaces it. Absent means progressive.
	interlaced: z.optional(z.boolean()),

	// The maximum frame rate of the video in frames per second.
	framerate: z.optional(z.number()),

	// The most frames that can precede a frame in decode order and follow it in presentation order,
	// as H.264 max_num_reorder_frames and HEVC sps_max_num_reorder_pics count them. 0 declares that
	// presentation order is decode order. Absent declares nothing: a consumer that authors decode
	// times infers them from the timestamps instead. Once declared, it holds for the track's life.
	reorder: z.optional(u8Schema),

	// The maximum bitrate of the video in bits per second.
	// TODO: Support up to Number.MAX_SAFE_INTEGER
	bitrate: z.optional(u53Schema),

	// Whether this rendition may be selected. When false, no frames are coming and a consumer
	// should not select it, though one may keep it selected while no enabled alternative exists.
	// Default: true, so publishers only write it when false.
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

	// This rendition's HLS segment records, when the publisher opted in. Without it the rendition
	// is not HLS-renderable. A malformed value is absent, so it can never reject the media.
	hls: HlsFieldSchema,
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

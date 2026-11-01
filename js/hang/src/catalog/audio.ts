import * as z from "@zod/mini";
import { compareName, compareNumber } from "./compare";
import { ContainerSchema } from "./container";
import { hexSchema } from "./hex";
import { HlsFieldSchema } from "./hls";
import { u53Schema } from "./integers";
import { EpochSchema, RelativeBroadcastSchema } from "./path";
import { codecNumber, countSchema, renditions } from "./section";

/**
 * Whether `codec` parses: an AAC string names its object type, while any other string is carried
 * as a codec this build may not decode. Mirrors the Rust `AudioCodec` parser.
 */
function codecValid(codec: string): boolean {
	if (codec.startsWith("mp4a.40.")) return codecNumber(codec.slice("mp4a.40.".length), 10, 0xff);
	return true;
}

const AudioConfigFields = z.object({
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

	// Registered WebCodecs codec string, or Hang's "pcm" extension for
	// interleaved little-endian IEEE-754 binary32 samples.
	codec: z.string().check(z.refine(codecValid, "malformed codec string")),

	// The container format, used to decode the timestamp and more.
	container: ContainerSchema,

	// The description is used for some codecs.
	// If provided, we can initialize the decoder based on the catalog alone.
	// Otherwise, the initialization information is in-band.
	description: z.optional(hexSchema),

	// The sample rate of the audio in Hz, never zero.
	sampleRate: countSchema,

	// The number of channels in the audio, never zero.
	numberOfChannels: countSchema,

	// What the decoded channels carry. Absent means the codec's own channel order. "ambisonic" is
	// full-sphere AmbiX (ACN order, SN3D normalization) whose order follows from numberOfChannels,
	// a square of at least 4. Typed as a plain string so an unrecognized layout survives a
	// republish; a consumer treats one as absent.
	layout: z.optional(z.string()),

	// The maximum bitrate of the audio in bits per second.
	// TODO: Support up to Number.MAX_SAFE_INTEGER
	bitrate: z.optional(u53Schema),

	// Whether this rendition may be selected. When false, no frames are coming and a consumer
	// should not select it, though one may keep it selected while no enabled alternative exists.
	// Default: true, so publishers only write it when false.
	enabled: z.optional(z.boolean()),

	// The maximum delay between a frame being ready and the publisher flushing it, in whole
	// milliseconds rounded up. The player's jitter buffer should be larger than this value.
	// If not provided, the player should assume each frame is flushed immediately.
	//
	// This is measured at the publisher (encoder latency, packet packing, reordering),
	// never on the network a consumer sees. It only ever grows over the life of a stream.
	//
	// NOTE: The audio "frame" duration depends on the codec, sample rate, etc.
	// ex: AAC often uses 1024 samples per frame, so at 44100Hz, this would be 1024/44100 = 24ms
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
 * Schema for a single audio rendition's decoder config.
 * Mirrors WebCodecs AudioDecoderConfig (https://w3c.github.io/webcodecs/#audio-decoder-config).
 */
export const AudioConfigSchema = AudioConfigFields.check(
	z.refine((c) => c.layout !== "ambisonic" || isAmbisonic(c.numberOfChannels), {
		message: "ambisonic audio needs a square channel count of at least 4",
	}),
);

// (order + 1)² channels for an order of at least 1.
function isAmbisonic(channels: number): boolean {
	const order = Math.round(Math.sqrt(channels));
	return order >= 2 && order * order === channels;
}

/** Schema for the catalog audio section: a map of track name to rendition config. */
export const AudioSchema = z.object({
	// A map of track name to rendition configuration; a malformed rendition refuses the catalog.
	// This is not an array so it will work with JSON Merge Patch.
	renditions: renditions(AudioConfigSchema),
});

/** The catalog audio section: renditions keyed by track name. */
export type Audio = z.infer<typeof AudioSchema>;
/** Decoder config for a single audio rendition. */
export type AudioConfig = z.infer<typeof AudioConfigSchema>;

/** Rank audio best first: enabled, then bitrate, sample rate, channels, and name. */
export function ranked(renditions: Record<string, AudioConfig>): [string, AudioConfig][] {
	return Object.entries(renditions).sort(
		(left, right) =>
			compareNumber(Number(left[1].enabled !== false), Number(right[1].enabled !== false)) ||
			compareNumber(left[1].bitrate, right[1].bitrate) ||
			compareNumber(left[1].sampleRate, right[1].sampleRate) ||
			compareNumber(left[1].numberOfChannels, right[1].numberOfChannels) ||
			compareName(left[0], right[0]),
	);
}

import { z } from "zod";
import { ContainerSchema } from "./container";
import { u53Schema } from "./integers";

// Backwards compatibility: old track schema
const TrackSchema = z.object({
	name: z.string(),
	priority: z.number().int().min(0).max(255),
});

// Mirrors AudioDecoderConfig
// https://w3c.github.io/webcodecs/#audio-decoder-config
export const AudioConfigSchema = z.object({
	// See: https://w3c.github.io/webcodecs/codec_registry.html
	codec: z.string(),

	// The container format, used to decode the timestamp and more.
	container: ContainerSchema,

	// The description is used for some codecs.
	// If provided, we can initialize the decoder based on the catalog alone.
	// Otherwise, the initialization information is in-band.
	description: z.string().optional(), // hex encoded TODO use base64

	// The sample rate of the audio in Hz
	sampleRate: u53Schema,

	// The number of channels in the audio
	numberOfChannels: u53Schema,

	// The bitrate of the audio in bits per second
	// TODO: Support up to Number.MAX_SAFE_INTEGER
	bitrate: u53Schema.optional(),

	// Minimum buffer size in milliseconds required for smooth playback.
	// This represents the minimum time the player should buffer before starting playback.
	// The player should add additional buffer on top of this value.
	minBuffer: u53Schema.optional(),
});

export const AudioSchema = z
	.object({
		// A map of track name to rendition configuration.
		// This is not an array so it will work with JSON Merge Patch.
		renditions: z.record(z.string(), AudioConfigSchema),

		// The priority of the audio track, relative to other tracks in the broadcast.
		priority: z.number().int().min(0).max(255),
	})
	.or(
		// Backwards compatibility: transform old {track, config} format to new object format
		z
			.object({
				track: TrackSchema,
				config: AudioConfigSchema,
			})
			.transform((old) => ({
				renditions: { [old.track.name]: old.config },
				priority: old.track.priority,
			})),
	);

export type Audio = z.infer<typeof AudioSchema>;
export type AudioConfig = z.infer<typeof AudioConfigSchema>;

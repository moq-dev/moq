import type * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";

// AudioWorklet always renders in 128-sample quanta.
const WORKLET_QUANTUM = 128;
const OPUS_FRAME_DURATION_MS = 20;
const AAC_LC_FRAME_SAMPLES = 1024;
const MP3_MPEG1_FRAME_SAMPLES = 1152;
const MP3_MPEG2_FRAME_SAMPLES = 576;
const MP3_MPEG1_MIN_SAMPLE_RATE = 32000;

/** The catalog fields that determine the demuxer and WebCodecs decoder instance. */
export type DecoderConfig = Pick<
	Catalog.AudioConfig,
	"codec" | "container" | "description" | "sampleRate" | "numberOfChannels"
>;

/** The routing and decoder state whose changes require a replacement subscription. */
export type PlaybackIdentity = {
	broadcast: Catalog.AudioConfig["broadcast"];
	decoder: DecoderConfig;
};

/** Reduce a rendition config to the fields that require a new decoder. */
export function decoderConfig(config: Catalog.AudioConfig): DecoderConfig {
	return {
		codec: config.codec,
		container: config.container,
		description: config.description,
		sampleRate: config.sampleRate,
		numberOfChannels: config.numberOfChannels,
	};
}

/** Reduce a rendition config to the fields that require a replacement subscription. */
export function playbackIdentity(config: Catalog.AudioConfig): PlaybackIdentity {
	return {
		broadcast: config.broadcast,
		decoder: decoderConfig(config),
	};
}

/** The jitter to add to the sync buffer for a rendition, in milliseconds. */
export function playbackJitter(config: Catalog.AudioConfig): Time.Milli {
	// A publisher advertising 0 is claiming frames are never delayed, which no encoder can do, so
	// fall back to the codec's frame duration the same way an absent field does.
	const codecJitter = (config.jitter || defaultJitter(config)) ?? 0;

	// Add the worklet render quantum so the ring buffer has margin between frame arrivals.
	const overhead = Math.ceil((WORKLET_QUANTUM / config.sampleRate) * 1000);
	return Time.Milli(codecJitter + overhead);
}

// Estimate the minimum jitter (frame duration) based on the audio codec.
// TODO these are defaults; the actual frame duration depends on encoder config.
function defaultJitter(config: Catalog.AudioConfig): number | undefined {
	if (config.codec.startsWith("opus")) {
		// Opus supports 2.5–60ms but 20ms is the real-time default.
		return OPUS_FRAME_DURATION_MS;
	}

	if (config.codec.startsWith("mp4a")) {
		// 1024 samples for LC-AAC; HE-AAC/AAC-LD use different sizes.
		return Math.ceil((AAC_LC_FRAME_SAMPLES / config.sampleRate) * 1000);
	}

	if (config.codec === "mp3") {
		// 1152 samples per frame for MPEG-1 Layer III; MPEG-2/2.5 use 576.
		const samples =
			config.sampleRate >= MP3_MPEG1_MIN_SAMPLE_RATE ? MP3_MPEG1_FRAME_SAMPLES : MP3_MPEG2_FRAME_SAMPLES;
		return Math.ceil((samples / config.sampleRate) * 1000);
	}

	return undefined;
}

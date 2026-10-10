import type * as Catalog from "@moq/hang/catalog";
import type * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import { Time } from "@moq/net";

const OPUS_RATE = 48_000;
const AAC_LC_FRAME_SAMPLES = 1024;
const AAC_HE_FRAME_SAMPLES = 2048;
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

/**
 * A frame's duration as a constant over the catalog's sample rate, for codecs that fix it.
 *
 * Undefined for Opus, whose packets state their own duration (see `Util.Opus.packetSamples`), and
 * for codecs with no constant. Never learned from observed timestamps.
 */
export function frameDuration(config: Pick<Catalog.AudioConfig, "codec" | "sampleRate">): Time.Milli | undefined {
	const samples = frameSamples(config);
	if (samples === undefined) return undefined;
	return Time.Milli((samples * 1000) / config.sampleRate);
}

/**
 * A frame's own duration, when it states one: the container's per-sample duration (CMAF), else the
 * duration an Opus packet declares in its TOC byte (RFC 6716 §3.1), always reckoned at 48 kHz.
 * CMAF reports an implicit duration as zero, which states nothing.
 */
export function packetDuration(codec: string, frame: Container.Frame): Time.Milli | undefined {
	if (frame.duration) return Time.Milli.fromMicro(frame.duration);
	if (!codec.startsWith("opus")) return undefined;
	const samples = Util.Opus.packetSamples(frame.payload);
	return samples === undefined ? undefined : Time.Milli((samples * 1000) / OPUS_RATE);
}

function frameSamples(config: Pick<Catalog.AudioConfig, "codec" | "sampleRate">): number | undefined {
	// HE-AAC (v1 and v2) doubles the output rate of its AAC-LC core.
	if (config.codec === "mp4a.40.5" || config.codec === "mp4a.40.29") return AAC_HE_FRAME_SAMPLES;
	if (config.codec.startsWith("mp4a")) return AAC_LC_FRAME_SAMPLES;
	if (config.codec === "mp3") {
		// MPEG-1 Layer III has 1152 samples per frame; MPEG-2/2.5 have 576.
		return config.sampleRate >= MP3_MPEG1_MIN_SAMPLE_RATE ? MP3_MPEG1_FRAME_SAMPLES : MP3_MPEG2_FRAME_SAMPLES;
	}
	return undefined;
}

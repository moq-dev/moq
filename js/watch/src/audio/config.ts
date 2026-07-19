import type * as Catalog from "@moq/hang/catalog";
import * as Util from "@moq/hang/util";

/** Return the decoder output rate for a catalog config, normalizing non-native Opus rates. */
export function audioDecoderSampleRate(config: Catalog.AudioConfig): number {
	return config.codec === "opus" ? Util.Opus.normalizeSampleRate(config.sampleRate) : config.sampleRate;
}

/** Build the exact WebCodecs audio decoder config used for support probes and playback. */
export function audioDecoderConfig(config: Catalog.AudioConfig, description?: Uint8Array): AudioDecoderConfig {
	return {
		codec: config.codec,
		sampleRate: audioDecoderSampleRate(config),
		numberOfChannels: config.numberOfChannels,
		description,
	};
}

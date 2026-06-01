import type * as Catalog from "@moq/hang/catalog";
import type { Kind } from "./types";

// `application`, `signal`, and `usedtx` are in the WebCodecs spec but missing from lib.dom.d.ts.
// https://www.w3.org/TR/webcodecs-opus-codec-registration/#dom-opusencoderconfig
interface OpusEncoderConfigExt extends OpusEncoderConfig {
	application?: "voip" | "audio" | "lowdelay";
	signal?: "auto" | "voice" | "music";
	usedtx?: boolean;
}

// Build the WebCodecs encoder config from the catalog (decoder) config plus a Kind hint.
// Opus-only knobs are kept out of the catalog since they only affect encoding.
// DTX is enabled for voice: speech has natural silence gaps where DTX emits tiny comfort-noise
// packets instead of full frames. Music has no useful silence to suppress.
export function toEncoderConfig(config: Catalog.AudioConfig, kind: Kind): AudioEncoderConfig {
	const encoderConfig: AudioEncoderConfig = {
		codec: config.codec,
		sampleRate: config.sampleRate,
		numberOfChannels: config.numberOfChannels,
		bitrate: config.bitrate,
	};

	if (config.codec === "opus" && kind !== "auto") {
		const opus: OpusEncoderConfigExt = {
			application: kind === "voice" ? "voip" : "audio",
			signal: kind,
			usedtx: kind === "voice",
		};
		encoderConfig.opus = opus;
	}

	return encoderConfig;
}

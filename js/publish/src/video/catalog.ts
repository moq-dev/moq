import * as Catalog from "@moq/hang/catalog";

/**
 * Build the catalog video config from the requested encoder config plus, when present, the decoder
 * config the encoder actually produced. Carries the out-of-band `description` (hvcC/av1C) some codecs
 * need, and only then adopts the encoder's codec string (a hev1 request can come back as hvc1, whose
 * length-prefixed bitstream the codec string must match). For in-band formats (H.264/HEVC annexb, VP8,
 * VP9, or before the first keyframe) it keeps the REQUESTED codec, so the catalog stays byte-identical
 * to what was requested and a watcher never rebuilds its decoder over a cosmetic codec-string change.
 * The captured value is applied only while its requested codec + dimensions still match the current
 * config. That is stable across the send-bandwidth-driven bitrate changes that rebuild the config object
 * ~10x/s (so a plain re-encode never flaps the description off), yet a real codec/resolution change
 * invalidates it until the next keyframe recaptures.
 *
 * Intentionally not re-exported from ./index (video/index.ts), so it stays an internal detail that
 * unit tests can pin without widening the package's public API (same as ./codecs).
 */
export function videoCatalog(
	config: VideoEncoderConfig,
	decoderConfig?: { reqCodec: string; width: number; height: number; codec: string; description?: string },
): Catalog.VideoConfig {
	// Apply the captured value only while the FORMAT-determining fields (requested codec + dimensions)
	// still match. Bitrate/framerate churn (bandwidth adaptation rebuilds #config ~10x/s) does not
	// invalidate the parameter sets; a codec or resolution change does.
	const dc =
		decoderConfig &&
		decoderConfig.reqCodec === config.codec &&
		decoderConfig.width === config.width &&
		decoderConfig.height === config.height
			? decoderConfig
			: undefined;
	return {
		// Only the description-bearing case (hvc1/av1C) needs the encoder's codec; otherwise the requested
		// codec is authoritative enough and avoids a mid-stream codec change on the common path.
		codec: dc?.description ? dc.codec : config.codec,
		description: dc?.description,
		bitrate: config.bitrate ? Catalog.u53(config.bitrate) : undefined,
		framerate: config.framerate,
		codedWidth: Catalog.u53(config.width),
		codedHeight: Catalog.u53(config.height),
		optimizeForLatency: true,
		container: { kind: "legacy" } as const,
		// Each frame is flushed immediately, so the jitter is one frame duration.
		jitter: config.framerate ? Catalog.u53(Math.ceil(1000 / config.framerate)) : undefined,
	};
}

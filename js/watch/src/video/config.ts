import type * as Catalog from "@moq/hang/catalog";

/** Internal key used to describe which fields can change a support probe's result. */
export const supportCacheKey = Symbol("supportCacheKey");

/** The catalog fields that determine the demuxer and WebCodecs decoder instance. */
export type DecoderConfig = Pick<
	Catalog.VideoConfig,
	"codec" | "container" | "description" | "displayAspectWidth" | "displayAspectHeight"
> & {
	optimizeForLatency: boolean;
};

/** The routing and decoder state whose changes require a new playback pipeline. */
export type PlaybackIdentity = {
	broadcast: Catalog.VideoConfig["broadcast"];
	decoder: DecoderConfig;
};

/** Reduce a rendition config to the fields that require a new playback pipeline. */
export function playbackIdentity(config: Catalog.VideoConfig): PlaybackIdentity {
	return {
		broadcast: config.broadcast,
		decoder: {
			codec: config.codec,
			container: config.container,
			description: config.description,
			displayAspectWidth: config.displayAspectWidth,
			displayAspectHeight: config.displayAspectHeight,
			optimizeForLatency: config.optimizeForLatency ?? true,
		},
	};
}

/** Return a stable cache key for the fields used by the WebCodecs support probe. */
export function decoderConfigKey(config: Catalog.VideoConfig): string {
	return JSON.stringify(playbackIdentity(config).decoder);
}

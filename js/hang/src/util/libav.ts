let loading: Promise<boolean> | undefined;

// The WebCodecs registry's AAC codec strings. The polyfill refuses every "mp4a" string, so these
// reach libav's own AAC decoder through the polyfill's libav.js codec escape hatch instead.
const AAC = new Set(["mp4a.40.2", "mp4a.40.02", "mp4a.40.5", "mp4a.40.05", "mp4a.40.29", "mp4a.67"]);

/** Load the libav WebCodecs polyfill (Opus, plus AAC decoding) if AudioEncoder/AudioDecoder are missing. Resolves true once available. */
export async function polyfill(): Promise<boolean> {
	if (globalThis.AudioEncoder && globalThis.AudioDecoder) {
		return true;
	}

	if (!loading) {
		console.warn("using libav WebCodecs polyfill; performance may be degraded");

		// @moq/libav is libav.js's opus-af variant plus the AAC decoder, which no published variant has.
		// I forked libavjs-webcodecs-polyfill to fix its TypeScript packaging and planar audio.
		loading = Promise.all([import("@moq/libav"), import("@kixelated/libavjs-webcodecs-polyfill")]).then(
			async ([{ default: LibAV }, libav]) => {
				const native = globalThis.AudioDecoder;
				await libav.load({ LibAV, polyfill: true });
				if (native) return true;

				type Config = Parameters<InstanceType<typeof libav.AudioDecoder>["configure"]>[0];
				const aac = (config: Config) => typeof config.codec === "string" && AAC.has(config.codec);
				const route = (config: Config): Config =>
					aac(config) ? { ...config, codec: { libavjs: { codec: "aac" } } } : config;

				class AudioDecoder extends libav.AudioDecoder {
					override configure(config: Config): void {
						super.configure(route(config));
					}

					static override async isConfigSupported(config: Config) {
						const { supported } = await libav.AudioDecoder.isConfigSupported(route(config));
						return { supported, config };
					}
				}
				globalThis.AudioDecoder = AudioDecoder as unknown as typeof globalThis.AudioDecoder;
				return true;
			},
		);
	}
	return await loading;
}

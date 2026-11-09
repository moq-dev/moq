import { objectType } from "./aac";

let loading: Promise<boolean> | undefined;

/**
 * Load the libav WebCodecs polyfill (Opus, plus AAC decoding) if AudioEncoder/AudioDecoder are missing. Resolves true once available.
 *
 * Its worker and wasm load from `base`, the calling package's hosted directory, or from beside libav's own module when undefined.
 * The first call's `base` wins, since the page shares one polyfill.
 */
export async function polyfill(base?: URL): Promise<boolean> {
	// The library installs its own globals before its load resolves, so once loading, every caller
	// waits for it rather than seeing those globals and using an AudioDecoder that refuses AAC.
	if (loading !== undefined) return await loading;
	if (globalThis.AudioEncoder && globalThis.AudioDecoder) {
		return true;
	}

	loading = load(base);
	return await loading;
}

async function load(base: URL | undefined): Promise<boolean> {
	// Whether the browser decodes natively, read before the imports yield to anything that installs one.
	const native = globalThis.AudioDecoder;
	console.warn("using libav WebCodecs polyfill; performance may be degraded");

	// @moq/libav is libav.js's opus-af variant plus the AAC decoder, which no published variant has.
	// I forked libavjs-webcodecs-polyfill to fix its TypeScript packaging and planar audio.
	const [{ default: LibAV }, libav] = await Promise.all([
		import("@moq/libav"),
		import("@kixelated/libavjs-webcodecs-polyfill"),
	]);
	// libav.js otherwise loads its worker and wasm beside its own module, a path consumer bundlers move.
	// It joins `base` and the file name with "/", so drop the trailing one.
	const libavOptions = base ? { base: base.href.slice(0, -1) } : {};
	await libav.load({ LibAV, polyfill: true, libavOptions });
	if (native) return true;

	type Config = Parameters<InstanceType<typeof libav.AudioDecoder>["configure"]>[0];
	// The polyfill refuses every "mp4a" string, so the WebCodecs registry's AAC strings reach
	// libav's own AAC decoder through the polyfill's libav.js codec escape hatch instead.
	const aac = (config: Config) => typeof config.codec === "string" && objectType(config.codec) !== undefined;
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
}

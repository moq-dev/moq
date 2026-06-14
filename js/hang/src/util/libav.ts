let loading: Promise<boolean> | undefined;

/** Load the libav WebCodecs polyfill (Opus) if AudioEncoder/AudioDecoder are missing. Resolves true once available. */
export async function polyfill(): Promise<boolean> {
	if (globalThis.AudioEncoder && globalThis.AudioDecoder) {
		return true;
	}

	if (!loading) {
		console.warn("using Opus polyfill; performance may be degraded");

		// Load the polyfill and the libav variant we're using.
		// TODO build with AAC support.
		// I forked libavjs-webcodecs-polyfill to avoid Typescript errors; there's no changes otherwise.
		loading = Promise.all([
			import("@libav.js/variant-opus-af"),
			import("@kixelated/libavjs-webcodecs-polyfill"),
		]).then(async ([opus, libav]) => {
			await libav.load({
				LibAV: opus,
				polyfill: true,
			});
			return true;
		});
	}
	return await loading;
}

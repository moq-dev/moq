import type { Plugin } from "vite";

const HEADERS = {
	"Cross-Origin-Opener-Policy": "same-origin",
	"Cross-Origin-Embedder-Policy": "require-corp",
};

/**
 * A Vite plugin that serves dev and preview pages cross-origin isolated.
 *
 * SharedArrayBuffer is gated behind cross-origin isolation, and without it the audio
 * renderer falls back to shipping samples over postMessage, which is both higher latency
 * and a different code path than a deployed site gets. Every asset the demos load is
 * same-origin, so `require-corp` costs nothing here and unlike `credentialless` it also
 * works in Safari.
 */
export function crossOriginIsolation(): Plugin {
	return {
		name: "moq-cross-origin-isolation",
		config: () => ({ server: { headers: HEADERS }, preview: { headers: HEADERS } }),
	};
}

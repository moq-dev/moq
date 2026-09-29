/**
 * Vite config for the audio quality page. `@moq/watch` is consumed as workspace source, so its render
 * worklet is inlined as a blob URL by the same plugin its own build uses, along with the harness's tap
 * worklet: a worklet fetched over the network would be one more thing that can stall inside the
 * measurement.
 *
 * `base: "./"` because one build is served under both `/isolated/` and `/plain/`.
 *
 * @module
 */
import { defineConfig } from "vite";
import { workletInline } from "../../../../js/common/vite-plugin-worklet";

/**
 * esnext keeps WebCodecs / WebTransport syntax intact for headless Chromium. Three pages: the player
 * under measurement, and the recorder and microphone publisher that `record.ts` drives.
 */
export default defineConfig({
	base: "./",
	plugins: [workletInline()],
	build: {
		target: "esnext",
		outDir: "dist",
		rollupOptions: { input: ["index.html", "recorder.html", "mic.html"] },
	},
});

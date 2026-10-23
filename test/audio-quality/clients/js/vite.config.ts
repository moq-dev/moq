/**
 * Vite config for the audio quality page. `@moq/watch` is consumed as workspace source, so its render
 * worklet is built by the same plugin its own build uses, along with the harness's tap worklet: both
 * load as blob URLs from lazy same-origin chunks, fetched once over loopback before the graded window.
 *
 * @module
 */
import { defineConfig } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet.ts";

/**
 * esnext keeps WebCodecs / WebTransport syntax intact for headless Chromium. Three pages: the player
 * under measurement, and the recorder and microphone publisher that `record.ts` drives.
 */
export default defineConfig({
	plugins: [worklet()],
	build: {
		target: "esnext",
		outDir: "dist",
		rollupOptions: { input: ["index.html", "recorder.html", "mic.html"] },
	},
});

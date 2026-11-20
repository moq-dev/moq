/**
 * Vite config for the browser interop client. `@moq/publish` is consumed here as
 * workspace *source*, not the prebuilt npm package, so its audio capture worklet
 * (`./capture-worklet.ts?worklet`) is not pre-inlined; the same plugin
 * `@moq/publish` uses for its own build inlines it as a blob URL here too.
 *
 * @module
 */

import { worklet } from "@moq/common/vite-plugin-worklet";
import { defineConfig } from "vite";

/** esnext keeps WebCodecs / WebTransport syntax intact for headless Chromium. */
export default defineConfig({
	plugins: [worklet()],
	build: { target: "esnext", outDir: "dist" },
});

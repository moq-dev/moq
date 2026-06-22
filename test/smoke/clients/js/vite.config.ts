/**
 * Vite config for the browser smoke client. `@moq/publish` is consumed here as
 * workspace *source*, not the prebuilt npm package, so its audio capture worklet
 * (`./capture-worklet.ts?worklet`) is not pre-inlined; the same plugin
 * `@moq/publish` uses for its own build inlines it as a blob URL here too.
 *
 * @module
 */
import { defineConfig } from "vite";
import { workletInline } from "../../../../js/common/vite-plugin-worklet";

/** esnext keeps WebCodecs / WebTransport syntax intact for headless Chromium. */
export default defineConfig({
	plugins: [workletInline()],
	build: { target: "esnext", outDir: "dist" },
});

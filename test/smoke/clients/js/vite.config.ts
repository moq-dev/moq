import { defineConfig } from "vite";
// @moq/publish is consumed here as workspace *source*, not the prebuilt npm
// package, so its audio capture worklet (`./capture-worklet.ts?worklet`) is not
// pre-inlined. The same plugin @moq/publish uses for its own build compiles and
// inlines it as a blob URL here too.
import { workletInline } from "../../../../js/common/vite-plugin-worklet";

// esnext keeps WebCodecs / WebTransport syntax intact for headless Chromium.
export default defineConfig({
	plugins: [workletInline()],
	build: { target: "esnext", outDir: "dist" },
});

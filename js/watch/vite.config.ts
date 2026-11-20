import { libav } from "@moq/common/vite-plugin-libav";
import { worklet } from "@moq/common/vite-plugin-worklet";
import { resolve } from "path";
import { defineConfig } from "vite";

export default defineConfig({
	plugins: [worklet(), libav()],
	build: {
		lib: {
			entry: {
				index: resolve(import.meta.dirname, "src/index.ts"),
				element: resolve(import.meta.dirname, "src/element.ts"),
				"ui/element": resolve(import.meta.dirname, "src/ui/element.ts"),
				"support/index": resolve(import.meta.dirname, "src/support/index.ts"),
				"support/element": resolve(import.meta.dirname, "src/support/element.ts"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			// Keep every published @moq dep (and its subpaths, e.g. @moq/signals/dom) external
			// so consumers dedupe against their own copy instead of bundling ours. The private
			// @moq/ui is never published, so it is bundled.
			// media-captions too: its browser build reads `window.VTTCue` on load, and only
			// an external import lets a server runtime pick its `node` export instead. Its
			// stylesheets stay bundled, since they are inlined as strings.
			external: (id) => (id.startsWith("@moq/") && id !== "@moq/ui") || id === "media-captions",
		},
		sourcemap: true,
		target: "esnext",
	},
});

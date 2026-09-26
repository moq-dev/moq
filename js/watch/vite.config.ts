import { resolve } from "path";
import { defineConfig } from "vite";
import { workletInline } from "../common/vite-plugin-worklet";

export default defineConfig({
	plugins: [workletInline()],
	build: {
		lib: {
			entry: {
				index: resolve(__dirname, "src/index.ts"),
				element: resolve(__dirname, "src/element.ts"),
				"ui/element": resolve(__dirname, "src/ui/element.ts"),
				"support/index": resolve(__dirname, "src/support/index.ts"),
				"support/element": resolve(__dirname, "src/support/element.ts"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			// Keep every @moq workspace dep (and its subpaths, e.g. @moq/signals/dom)
			// external so consumers dedupe against their own copy instead of bundling ours.
			// media-captions too: its browser build reads `window.VTTCue` on load, and only
			// an external import lets a server runtime pick its `node` export instead. Its
			// stylesheets stay bundled, since they are inlined as strings.
			external: (id) => id.startsWith("@moq/") || id === "media-captions",
		},
		sourcemap: true,
		target: "esnext",
	},
});

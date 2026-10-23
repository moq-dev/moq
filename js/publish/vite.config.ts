import { resolve } from "path";
import { defineConfig } from "vite";
import { worklet } from "../common/vite-plugin-worklet.ts";
import { lazyMedia } from "./bundle.ts";

export default defineConfig({
	plugins: [worklet(), lazyMedia()],
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
			// Keep every @moq workspace dep (and its subpaths, e.g. @moq/signals/dom)
			// external so consumers dedupe against their own copy instead of bundling ours.
			external: (id) => id.startsWith("@moq/"),
		},
		sourcemap: true,
		target: "esnext",
	},
});

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
				preview: resolve(__dirname, "src/preview.ts"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			external: ["@moq/hang", "@moq/lite", "@moq/signals", "@moq/watch"],
		},
		sourcemap: true,
		target: "esnext",
	},
});

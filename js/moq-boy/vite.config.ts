import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { workletInline } from "../common/vite-plugin-worklet";

export default defineConfig({
	plugins: [solidPlugin(), workletInline()],
	build: {
		lib: {
			entry: {
				index: resolve(__dirname, "src/index.ts"),
				element: resolve(__dirname, "src/element.tsx"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			external: ["@moq/lite", "@moq/signals", "@moq/ui-core", "@moq/watch"],
		},
		sourcemap: true,
		target: "esnext",
	},
});

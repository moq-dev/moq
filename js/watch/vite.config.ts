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
				element: resolve(__dirname, "src/element.ts"),
				"ui/index": resolve(__dirname, "src/ui/index.tsx"),
				"support/index": resolve(__dirname, "src/support/index.ts"),
				"support/element": resolve(__dirname, "src/support/element.ts"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			external: ["@moq/hang", "@moq/lite", "@moq/signals", "@moq/ui-core"],
		},
		sourcemap: true,
		target: "esnext",
	},
});

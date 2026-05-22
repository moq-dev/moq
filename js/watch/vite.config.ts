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
			external: ["@moq/hang", "@moq/net", "@moq/signals"],
		},
		sourcemap: true,
		target: "esnext",
	},
});

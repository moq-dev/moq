import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	plugins: [solidPlugin()],
	build: {
		lib: {
			entry: {
				index: resolve(__dirname, "src/index.ts"),
				element: resolve(__dirname, "src/element.ts"),
				"ui/index": resolve(__dirname, "src/ui/index.tsx"),
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

import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	plugins: [solidPlugin()],
	build: {
		lib: {
			entry: {
				publish: resolve(__dirname, "src/Components/publish/index.tsx"),
				watch: resolve(__dirname, "src/Components/watch/index.tsx"),
				stats: resolve(__dirname, "src/Components/stats/index.tsx"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			external: ["solid-js", "solid-element", "@moq/hang", "@moq/signals"],
		},
		sourcemap: true,
		target: "esnext",
	},
});

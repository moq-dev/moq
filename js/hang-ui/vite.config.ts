import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	plugins: [solidPlugin()],
	build: {
		lib: {
			entry: {
				publish: resolve(__dirname, "src/publish.tsx"),
				watch: resolve(__dirname, "src/watch.tsx"),
				stats: resolve(__dirname, "src/stats.tsx"),
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

import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	plugins: [solidPlugin()],
	build: {
		lib: {
			entry: {
				index: resolve(import.meta.dirname, "src/index.ts"),
				element: resolve(import.meta.dirname, "src/element.tsx"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			external: ["@moq/net", "@moq/signals", "@moq/watch"],
		},
		sourcemap: true,
		target: "esnext",
	},
});

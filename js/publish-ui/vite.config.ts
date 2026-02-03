import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	plugins: [solidPlugin()],
	build: {
		lib: {
			entry: {
				index: resolve(__dirname, "src/publish/index.tsx"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			external: ["@moq/publish", "@moq/lite", "@moq/signals"],
		},
		sourcemap: true,
		target: "esnext",
	},
});

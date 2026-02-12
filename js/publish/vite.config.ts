import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	plugins: [solidPlugin()],
	build: {
		lib: {
			entry: {
				"ui/index": resolve(__dirname, "src/ui/index.tsx"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			external: ["@moq/hang", "@moq/lite", "@moq/signals", "@moq/ui-core"],
		},
		outDir: "dist",
		emptyOutDir: false,
		sourcemap: true,
		target: "esnext",
	},
});

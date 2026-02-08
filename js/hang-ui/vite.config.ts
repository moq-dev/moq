import { copyFileSync } from "fs";
import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	plugins: [
		solidPlugin(),
		{
			name: "copy-dts",
			writeBundle() {
				copyFileSync(resolve(__dirname, "src/watch/watch.d.ts"), resolve(__dirname, "dist/watch/watch.d.ts"));
				copyFileSync(
					resolve(__dirname, "src/publish/publish.d.ts"),
					resolve(__dirname, "dist/publish/publish.d.ts"),
				);
			},
		},
	],
	build: {
		lib: {
			entry: {
				"publish/index": resolve(__dirname, "src/publish/index.tsx"),
				"watch/index": resolve(__dirname, "src/watch/index.tsx"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			external: ["@moq/hang", "@moq/lite", "@moq/signals"],
		},
		sourcemap: true,
		target: "esnext",
	},
});

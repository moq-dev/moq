import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { worklet } from "../common/vite-plugin-worklet";

export default defineConfig({
	plugins: [solidPlugin(), worklet()],
	build: {
		lib: {
			entry: {
				index: resolve(__dirname, "src/index.ts"),
				element: resolve(__dirname, "src/element.tsx"),
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

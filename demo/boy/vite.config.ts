import { worklet } from "@moq/common/vite-plugin-worklet";
import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	root: "src",
	envDir: resolve(import.meta.dirname),
	plugins: [solidPlugin(), worklet()],
	build: {
		target: "esnext",
		rollupOptions: {
			input: {
				main: resolve(import.meta.dirname, "src/index.html"),
			},
		},
	},
	server: {
		hmr: false,
	},
	optimizeDeps: {
		exclude: ["@moq/libav"],
	},
});

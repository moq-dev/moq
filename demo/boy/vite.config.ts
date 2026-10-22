import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { crossOriginIsolation } from "../../js/common/vite-plugin-isolate.ts";
import { worklet } from "../../js/common/vite-plugin-worklet.ts";

export default defineConfig({
	root: "src",
	envDir: resolve(import.meta.dirname),
	plugins: [solidPlugin(), worklet(), crossOriginIsolation()],
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

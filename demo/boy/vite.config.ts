import { resolve } from "path";
import { defineConfig } from "vite";
import { workletInline } from "../../js/common/vite-plugin-worklet";

export default defineConfig({
	root: "src",
	envDir: resolve(__dirname),
	plugins: [workletInline()],
	build: {
		target: "esnext",
		rollupOptions: {
			input: {
				main: resolve(__dirname, "src/index.html"),
			},
		},
	},
	server: {
		hmr: false,
	},
	optimizeDeps: {
		exclude: ["@libav.js/variant-opus-af"],
	},
});

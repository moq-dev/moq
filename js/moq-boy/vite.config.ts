import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { workletInline } from "../common/vite-plugin-worklet";

export default defineConfig({
	root: "src",
	envDir: resolve(__dirname),
	publicDir: false,
	plugins: [solidPlugin(), workletInline()],
	build: {
		target: "esnext",
		sourcemap: "inline",
	},
	server: {
		hmr: false,
	},
});

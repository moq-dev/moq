import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	root: "src",
	publicDir: false,
	plugins: [solidPlugin()],
	build: {
		target: "esnext",
		sourcemap: "inline",
	},
	server: {
		hmr: false,
	},
});

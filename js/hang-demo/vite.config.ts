import tailwindcss from "@tailwindcss/vite";
import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { workletInline } from "../common/vite-plugin-worklet";

export default defineConfig({
	root: "src",
	plugins: [tailwindcss(), solidPlugin(), workletInline()],
	build: {
		target: "esnext",
		sourcemap: process.env.NODE_ENV === "production" ? false : "inline",
		rollupOptions: {
			input: {
				watch: resolve(__dirname, "src/index.html"),
				publish: resolve(__dirname, "src/publish.html"),
				support: resolve(__dirname, "src/support.html"),
				mse: resolve(__dirname, "src/mse.html"),
			},
		},
	},
	server: {
		// TODO: properly support HMR
		hmr: false,
	},
	optimizeDeps: {
		// No idea why this needs to be done, but I don't want to figure it out.
		exclude: ["@libav.js/variant-opus-af"],
	},
});

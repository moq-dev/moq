import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
	root: "src",
	plugins: [tailwindcss(), solidPlugin()],
	build: {
		target: "esnext",
		sourcemap: process.env.NODE_ENV === "production" ? false : "inline",
		rollupOptions: {
			input: {
				watch: "index.html",
				"watch-mse": "watch-mse.html",
				publish: "publish.html",
				support: "support.html",
				meet: "meet.html",
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

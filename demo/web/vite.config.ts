import tailwindcss from "@tailwindcss/vite";
import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { workletInline } from "../../js/common/vite-plugin-worklet";
import { consoleOverlay } from "./console-overlay";
import { openTabs } from "./open-tabs";

export default defineConfig({
	root: "src",
	envDir: resolve(__dirname),
	plugins: [
		tailwindcss(),
		solidPlugin(),
		workletInline(),
		consoleOverlay(),
		// Open the stats, publish, and watch demos each in their own tab.
		// Order matters: the browser focuses the last tab, so watch ends up in front.
		openTabs(["stats.html", "publish.html", "watch.html"]),
	],
	build: {
		target: "esnext",
		sourcemap: process.env.NODE_ENV === "production" ? false : "inline",
		rollupOptions: {
			input: {
				watch: resolve(__dirname, "src/watch.html"),
				publish: resolve(__dirname, "src/publish.html"),
				stats: resolve(__dirname, "src/stats.html"),
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

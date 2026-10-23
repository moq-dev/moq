import tailwindcss from "@tailwindcss/vite";
import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { worklet } from "../../js/common/vite-plugin-worklet.ts";
import { consoleOverlay } from "./console-overlay.ts";
import { openTabs } from "./open-tabs.ts";

export default defineConfig({
	root: "src",
	envDir: resolve(import.meta.dirname),
	plugins: [
		tailwindcss(),
		solidPlugin(),
		worklet(),
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
				watch: resolve(import.meta.dirname, "src/watch.html"),
				publish: resolve(import.meta.dirname, "src/publish.html"),
				stats: resolve(import.meta.dirname, "src/stats.html"),
				meet: resolve(import.meta.dirname, "src/meet.html"),
			},
		},
	},
	server: {
		// TODO: properly support HMR
		hmr: false,
	},
	optimizeDeps: {
		// No idea why this needs to be done, but I don't want to figure it out.
		exclude: ["@moq/libav"],
	},
});

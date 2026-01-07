import tailwindcss from "@tailwindcss/vite";
import path from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { viteStaticCopy } from "vite-plugin-static-copy";

export default defineConfig({
	root: "src",
	plugins: [
		tailwindcss(),
		solidPlugin(),
		viteStaticCopy({
			targets: [
				{
					// NOTE: When using the NPM package, you instead use:
					// src: "node_modules/@moq/hang-ui/dist/assets/*"
					src: path.resolve(__dirname, "node_modules/@moq/hang-ui/src/assets/*"),
					dest: "@moq/hang-ui",
				},
			],
		}),
	],
	build: {
		target: "esnext",
		sourcemap: process.env.NODE_ENV === "production" ? false : "inline",
		rollupOptions: {
			input: {
				watch: "index.html",
				publish: "publish.html",
				support: "support.html",
				meet: "meet.html",
			},
		},
	},
	server: {
		hmr: false,
	},
	optimizeDeps: {
		include: ["@moq/hang-ui"],
		exclude: ["@libav.js/variant-opus-af"],
	},
});

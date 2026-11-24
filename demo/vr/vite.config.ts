import { worklet } from "@moq/common/vite-plugin-worklet";
import { resolve } from "path";
import { defineConfig } from "vite";

export default defineConfig({
	root: "src",
	envDir: resolve(import.meta.dirname),
	plugins: [worklet()],
	build: { target: "esnext" },
	server: { hmr: false },
});

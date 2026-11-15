import { resolve } from "path";
import { defineConfig } from "vite";
import { worklet } from "../../js/common/vite-plugin-worklet.ts";

export default defineConfig({
	root: "src",
	envDir: resolve(import.meta.dirname),
	plugins: [worklet()],
	build: { target: "esnext" },
	server: { hmr: false },
});

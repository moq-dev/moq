import { defineConfig } from "vite";
import { worklet } from "../../../js/common/vite-plugin-worklet.ts";

export default defineConfig({
	plugins: [worklet()],
	build: { target: "esnext" },
	server: { hmr: false },
});

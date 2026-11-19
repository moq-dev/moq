import { worklet } from "@moq/common/vite-plugin-worklet";
import { defineConfig } from "vite";

export default defineConfig({
	plugins: [worklet()],
	build: { target: "esnext" },
	server: { hmr: false },
});

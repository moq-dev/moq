import { resolve } from "path";
import { defineConfig } from "vite";

export default defineConfig({
	build: {
		lib: {
			entry: {
				index: resolve(import.meta.dirname, "src/index.ts"),
			},
			formats: ["es"],
		},
		rollupOptions: {
			// Keep every @moq workspace dep (and its subpaths) external so consumers dedupe against their own copy.
			external: (id) => id.startsWith("@moq/"),
		},
		sourcemap: true,
		target: "esnext",
	},
});

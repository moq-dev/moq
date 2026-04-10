import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { workletInline } from "../common/vite-plugin-worklet";

// Produces a self-contained ESM bundle under dist/bundle/ with every
// dependency inlined. This is what jsDelivr/unpkg consumers load via a
// <script type="module"> tag — no bundler or import map required.
//
// The main vite.config.ts still builds the per-entry library output used by
// application bundlers that already resolve @moq/* packages themselves.
export default defineConfig({
	plugins: [solidPlugin(), workletInline()],
	build: {
		outDir: "dist/bundle",
		// dist/ was already cleaned by `rimraf dist` in the build script and
		// the sibling library build populated dist/ before we ran — don't wipe
		// it here.
		emptyOutDir: false,
		lib: {
			// A single entry that registers every <moq-publish*> custom element.
			entry: resolve(__dirname, "src/bundle.ts"),
			name: "MoqPublish",
			fileName: () => "moq-publish.js",
			formats: ["es"],
		},
		rollupOptions: {
			// No externals — everything (solid-js, @moq/*, etc.) must be inlined
			// so the bundle is self-contained.
			external: [],
			output: {
				// Keep the output a single file: inline dynamic imports (WebCodecs
				// polyfill, libav-opus fallback) so a CDN consumer only has to
				// fetch one script.
				inlineDynamicImports: true,
			},
		},
		sourcemap: true,
		target: "esnext",
		minify: "esbuild",
	},
});

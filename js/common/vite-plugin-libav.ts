import { readdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import type { Plugin } from "vite";

/**
 * A Vite plugin that ships `@moq/libav`'s worker and wasm in a library build's `assets/`, beside its worklets.
 *
 * A page under a strict CSP hosts a package's `assets/*` and passes that directory to the libav polyfill, so
 * every package that loads the polyfill carries the files it needs.
 */
export function libav(): Plugin {
	let root = "";

	return {
		name: "libav",
		apply: "build",

		configResolved(config) {
			root = config.root;
		},

		buildStart() {
			// Resolved by Node rather than this.resolve(), which honors the build's `@moq/*` externals.
			const dir = dirname(createRequire(join(root, "package.json")).resolve("@moq/libav"));
			const files = readdirSync(dir).filter((name) => name.includes(".wasm."));
			if (files.length === 0) throw new Error(`no libav worker or wasm in ${dir}`);
			for (const name of files) {
				this.emitFile({ type: "asset", fileName: `assets/${name}`, source: readFileSync(join(dir, name)) });
			}
		},
	};
}

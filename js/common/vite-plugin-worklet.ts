import { build } from "esbuild";
import type { Plugin } from "vite";

const SUFFIX = "?worklet";

/**
 * A Vite plugin that compiles AudioWorklet files and inlines them as blob URLs.
 *
 * Usage: import workletUrl from "./my-worklet.ts?worklet"
 *
 * The worklet file is compiled to JS with all dependencies bundled via esbuild,
 * then inlined as a string. At runtime, a blob URL is created and exported.
 * Pass the URL to audioWorklet.addModule().
 */
export function workletInline(): Plugin {
	return {
		name: "worklet-inline",
		enforce: "pre",

		async resolveId(source, importer) {
			if (!source.endsWith(SUFFIX)) return;

			const cleanSource = source.slice(0, -SUFFIX.length);
			const resolved = await this.resolve(cleanSource, importer, { skipSelf: true });
			if (!resolved) return;

			return { id: resolved.id + SUFFIX, moduleSideEffects: false };
		},

		async load(id) {
			if (!id.endsWith(SUFFIX)) return;

			const filePath = id.slice(0, -SUFFIX.length);

			if (this.addWatchFile) {
				this.addWatchFile(filePath);
			}

			const result = await build({
				entryPoints: [filePath],
				bundle: true,
				write: false,
				format: "esm",
				target: "esnext",
			});

			const compiled = result.outputFiles[0].text;

			return [
				`const code = ${JSON.stringify(compiled)};`,
				`const blob = new Blob([code], { type: "application/javascript" });`,
				`export default URL.createObjectURL(blob);`,
			].join("\n");
		},
	};
}

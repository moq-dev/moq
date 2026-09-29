import { basename } from "node:path";
import { build } from "esbuild";
import type { Plugin } from "vite";

const SUFFIX = "?worklet";

/**
 * A Vite plugin that bundles AudioWorklets as files in builds and blob URLs in development.
 *
 * Usage: import workletUrl from "./my-worklet.ts?worklet"
 *
 * The worklet file is compiled to JS with all dependencies bundled via esbuild,
 * then emitted with a static import.meta.url reference that downstream bundlers can copy.
 * Pass the URL to audioWorklet.addModule().
 */
export function worklet(alias?: Record<string, string>): Plugin {
	let production = false;
	const assets = new Set<string>();
	return {
		name: "worklet",
		configResolved(config) {
			production = config.command === "build";
		},
		resolveFileUrl({ referenceId, relativePath }) {
			if (assets.has(referenceId)) return `new URL(${JSON.stringify(relativePath)}, import.meta.url).href`;
			return undefined;
		},
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
				alias: alias,
			});

			const compiled = result.outputFiles[0].text;
			if (production) {
				const reference = this.emitFile({
					type: "asset",
					name: basename(filePath).replace(/\.ts$/, ".js"),
					source: compiled,
				});
				assets.add(reference);
				return `export default import.meta.ROLLUP_FILE_URL_${reference};`;
			}

			return [
				`const code = ${JSON.stringify(compiled)};`,
				`const blob = new Blob([code], { type: "application/javascript" });`,
				`export default URL.createObjectURL(blob);`,
			].join("\n");
		},
	};
}

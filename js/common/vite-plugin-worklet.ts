import { basename } from "node:path";
import { build } from "esbuild";
import type { Plugin } from "vite";

const SUFFIX = "?worklet";
const BLOB = "?worklet-blob";

/**
 * A Vite plugin that bundles an AudioWorklet or Worker into a standalone script.
 *
 * Usage: import url from "./my-worklet.ts?worklet"; await addModule(await url(base));
 *
 * The default export resolves to `new URL("<name>.js", base)` when given a base, for pages whose CSP
 * refuses blob: and that host the file themselves. Builds emit it to `assets/<name>.js`. Without a
 * base it resolves to a blob: URL, whose inlined source sits behind a dynamic import so code-splitting
 * bundlers only fetch it when used. Never `import.meta.url`: many consumer bundlers drop the asset.
 */
export function worklet(alias?: Record<string, string>): Plugin {
	let production = false;

	const compile = async (path: string) => {
		const result = await build({
			entryPoints: [path],
			bundle: true,
			write: false,
			format: "esm",
			target: "esnext",
			// A consumer can't minify code inlined as a string, so builds do it here; dev stays readable.
			minify: production,
			alias: alias,
		});
		return result.outputFiles[0].text;
	};

	return {
		name: "worklet",
		enforce: "pre",

		configResolved(config) {
			production = config.command === "build";
		},

		async resolveId(source, importer) {
			const suffix = [SUFFIX, BLOB].find((suffix) => source.endsWith(suffix));
			if (!suffix) return;

			const resolved = await this.resolve(source.slice(0, -suffix.length), importer, { skipSelf: true });
			if (!resolved) return;

			return { id: resolved.id + suffix, moduleSideEffects: false };
		},

		async load(id) {
			if (id.endsWith(BLOB)) {
				const path = id.slice(0, -BLOB.length);
				this.addWatchFile(path);

				return [
					`const code = ${JSON.stringify(await compile(path))};`,
					`export default URL.createObjectURL(new Blob([code], { type: "text/javascript" }));`,
				].join("\n");
			}

			if (!id.endsWith(SUFFIX)) return;

			const path = id.slice(0, -SUFFIX.length);
			const name = basename(path).replace(/\.ts$/, ".js");

			if (production) {
				this.emitFile({ type: "asset", fileName: `assets/${name}`, source: await compile(path) });
			}

			return [
				`export default async (base) => {`,
				`	if (base) return new URL(${JSON.stringify(name)}, base).href;`,
				`	return (await import(${JSON.stringify(path + BLOB)})).default;`,
				`};`,
			].join("\n");
		},
	};
}

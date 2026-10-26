import { basename } from "node:path";
import { rolldown } from "rolldown";
import type { Plugin } from "vite";

const SUFFIX = "?worklet";
const BLOB = "?worklet-blob";

/**
 * A Vite plugin that bundles an AudioWorklet or Worker into a standalone script.
 *
 * Usage: import url from "./my-worklet.ts?worklet"; await addModule(await url(base));
 *
 * The default export resolves to `new URL("<name>.js", base)` when given a base, for pages whose CSP
 * refuses blob: and that host the file themselves. Builds emit it to `assets/<name>.js` and the dev
 * server serves it there. Without a base it resolves to a blob: URL, whose inlined source sits behind
 * a dynamic import so code-splitting bundlers only fetch it when used. Never `import.meta.url`: many
 * consumer bundlers drop the asset.
 */
export function worklet(alias?: Record<string, string>): Plugin {
	let production = false;
	// The dev server's hosted files, by `assets/<name>.js`, filled as their importers load.
	const hosted = new Map<string, string>();

	// Rolldown is Vite's own bundler and runs in-process, so nothing outlives the build.
	const compile = async (path: string) => {
		const bundle = await rolldown({ input: path, platform: "browser", resolve: { alias } });
		try {
			const { output } = await bundle.generate({
				format: "es",
				codeSplitting: false,
				// A consumer can't minify code inlined as a string, so builds do it here; dev stays readable.
				minify: production,
			});
			return output[0].code;
		} finally {
			await bundle.close();
		}
	};

	return {
		name: "worklet",
		enforce: "pre",

		configResolved(config) {
			production = config.command === "build";
		},

		configureServer(server) {
			server.middlewares.use(async (req, res, next) => {
				// A malformed target must reach Connect as an error, not reject the async handler.
				try {
					// Undo the percent-encoding the browser applies to a name such as `render worklet.js`.
					const pathname = decodeURI(new URL(req.url ?? "/", "http://localhost").pathname);
					const base = server.config.base;
					const path = pathname.startsWith(base) ? hosted.get(pathname.slice(base.length)) : undefined;
					if (!path) return next();

					const code = await compile(path);
					res.setHeader("Content-Type", "text/javascript");
					res.end(code);
				} catch (err) {
					next(err);
				}
			});
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
			} else {
				hosted.set(`assets/${name}`, path);
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

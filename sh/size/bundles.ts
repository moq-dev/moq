// Measure consumer bundles from published dist entry points, including only static chunks.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { brotliCompressSync, gzipSync } from "node:zlib";
import { build } from "esbuild";

const root = process.cwd();
if (!process.argv[2]) throw new Error("usage: bundles.ts OUTPUT");
const out = resolve(process.argv[2]);
mkdirSync(out, { recursive: true });
const sizes: Record<string, number> = {};
for (const [name, code] of Object.entries({
	net: 'export * from "@moq/net";',
	hang: 'export * from "@moq/hang"; export * from "@moq/hang/catalog"; export * from "@moq/hang/container"; export * from "@moq/hang/util";',
	watch: 'import "@moq/watch/element";',
	"watch-ui": 'import "@moq/watch/element"; import "@moq/watch/ui";',
	publish: 'import "@moq/publish/element";',
})) {
	const result = await build({
		stdin: { contents: code, resolveDir: root, sourcefile: `${name}.js` },
		bundle: true,
		minify: true,
		splitting: true,
		format: "esm",
		platform: "browser",
		metafile: true,
		write: false,
		outdir: join(out, name),
		plugins: [
			{
				name: "built-packages",
				setup(b) {
					b.onResolve({ filter: /^@moq\// }, ({ path }) => {
						const [, pkg, ...subpath] = path.split("/");
						const dir = join(root, "js", pkg, "dist");
						// Only packages outside this workspace fall through to node_modules.
						if (!existsSync(join(root, "js", pkg, "package.json"))) return;
						const manifest: { exports: Record<string, string | { default: string }> } = JSON.parse(
							readFileSync(join(dir, "package.json"), "utf8"),
						);
						const entry = manifest.exports[subpath.length ? `./${subpath.join("/")}` : "."];
						if (!entry) throw new Error(`missing built export: ${path}`);
						return { path: resolve(dir, typeof entry === "string" ? entry : entry.default) };
					});
				},
			},
		],
	});
	if (!result.metafile || !result.outputFiles) throw new Error(`bundle outputs missing: ${name}`);
	writeFileSync(join(out, `${name}.metafile.json`), JSON.stringify(result.metafile, null, 2));
	const outputs = result.metafile.outputs;
	const first = new Set<string>();
	const visit = (path: string) => {
		if (first.has(path)) return;
		first.add(path);
		for (const dep of outputs[path].imports) if (!dep.external && dep.kind !== "dynamic-import") visit(dep.path);
	};
	for (const [path, output] of Object.entries(outputs)) if (output.entryPoint === `${name}.js`) visit(path);
	if (!first.size) throw new Error(`entry output missing: ${name}`);
	// CMAF is an optional synchronous export, never part of these initial entry points.
	const hasCmaf = (path: string) =>
		Object.keys(outputs[path].inputs).some((input) => /[/]cmaf[/]/.test(input));
	if ([...first].some(hasCmaf)) throw new Error(`CMAF reached the first load: ${name}`);
	const lazyCmaf = new Set(Object.keys(outputs).filter((path) => !first.has(path) && hasCmaf(path)));
	if (name.startsWith("watch") && !lazyCmaf.size) throw new Error(`missing lazy CMAF chunk: ${name}`);
	for (const file of result.outputFiles) {
		const initial = [...first].some((path) => resolve(root, path) === file.path);
		const cmaf = [...lazyCmaf].some((path) => resolve(root, path) === file.path);
		if (!initial && !cmaf) continue;
		const label = initial ? name : `${name}/cmaf`;
		for (const [format, bytes] of Object.entries({
			raw: file.contents.length,
			gzip: gzipSync(file.contents).length,
			brotli: brotliCompressSync(file.contents).length,
		}))
			sizes[`js/${label}/${format}`] = (sizes[`js/${label}/${format}`] ?? 0) + bytes;
	}
	if (!sizes[`js/${name}/raw`]) throw new Error(`no first-load bytes: ${name}`);
}
writeFileSync(join(out, "bytes.json"), JSON.stringify(sizes, null, 2));

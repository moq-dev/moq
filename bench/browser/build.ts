/**
 * Bundles the benchmark pages once per variant.
 *
 * Every variant shares these page sources but resolves `@moq/*` against its own checkout, so a
 * base revision is measured through the same harness as the current tree.
 *
 *     bun build.ts <out> <name>=<root>[:copy] ...
 *
 * `:copy` injects one extra copy of every received stream chunk into @moq/net, the control
 * that proves the suite can see a copy-heavy change.
 */
import { basename, join, resolve } from "node:path";
import type { BunPlugin } from "bun";
import { workspace } from "./workspace.ts";

const PAGES = ["transport", "cmaf", "media"].map((page) => join(import.meta.dir, "page", `${page}.ts`));

// The one statement every received byte passes through; the control copies it.
const CHUNK = "this.#chunks.push(result.value);";
const COPIED = "this.#chunks.push(result.value.slice());";

const [out, ...specs] = process.argv.slice(2);
if (!out || specs.length === 0) throw new Error("usage: bun build.ts <out> <name>=<root>[:copy] ...");

for (const spec of specs) {
	const match = /^([a-z0-9-]+)=(.+?)(:copy)?$/.exec(spec);
	if (!match) throw new Error(`malformed variant: ${spec}`);
	const [, name, root, copy] = match;
	const result = await Bun.build({
		entrypoints: PAGES,
		outdir: join(out, name),
		target: "browser",
		plugins: [workspace(resolve(root)), worklet(), text(), ...(copy ? [inject(resolve(root))] : [])],
	});
	if (!result.success) throw new AggregateError(result.logs, `${name} failed to bundle`);
	await Bun.write(join(out, name, "index.html"), "<!doctype html><body></body>\n");
	console.log(`bundled ${name} from ${root}${copy ? " with an injected chunk copy" : ""}`);
}

/** `?worklet` imports become a blob URL of the separately bundled worklet, as vite-plugin-worklet does. */
function worklet(): BunPlugin {
	return {
		name: "worklet",
		setup(build) {
			build.onResolve({ filter: /\?worklet$/ }, (args) => ({
				path: resolve(args.resolveDir, args.path.slice(0, -"?worklet".length)),
				namespace: "worklet",
			}));
			build.onLoad({ filter: /.*/, namespace: "worklet" }, async (args) => {
				// A nested in-process Bun.build deadlocks inside a plugin, so the worklet bundles in a child.
				const child = Bun.spawnSync(["bun", "build", args.path, "--target", "browser"], { stderr: "pipe" });
				if (child.exitCode !== 0) throw new Error(`worklet ${args.path} failed to bundle: ${child.stderr}`);
				const code = child.stdout.toString();
				const name = basename(args.path).replace(/\.ts$/, ".js");
				return {
					loader: "js",
					contents: [
						`const code = ${JSON.stringify(code)};`,
						`export default async (base) => base ? new URL(${JSON.stringify(name)}, base).href`,
						`	: URL.createObjectURL(new Blob([code], { type: "text/javascript" }));`,
					].join("\n"),
				};
			});
		},
	};
}

/** Vite's `?raw` and `?inline` imports, which the watch and publish sources use for CSS and markup. */
function text(): BunPlugin {
	return {
		name: "text",
		setup(build) {
			build.onResolve({ filter: /\?(raw|inline)$/ }, (args) => ({
				path: Bun.resolveSync(args.path.replace(/\?(raw|inline)$/, ""), args.resolveDir),
				namespace: "text",
			}));
			build.onLoad({ filter: /.*/, namespace: "text" }, async (args) => ({
				loader: "js",
				contents: `export default ${JSON.stringify(await Bun.file(args.path).text())};`,
			}));
		},
	};
}

/** Copies each received chunk once more, failing loudly if the stream path moved. */
function inject(root: string): BunPlugin {
	const target = join(root, "js/net/src/stream.ts");
	return {
		name: "inject-copy",
		setup(build) {
			build.onLoad({ filter: /stream\.ts$/ }, async (args) => {
				if (args.path !== target) return undefined;
				const source = await Bun.file(target).text();
				if (source.split(CHUNK).length !== 2)
					throw new Error(`${target} no longer has exactly one \`${CHUNK}\`; move the injected copy`);
				return { loader: "ts", contents: source.replace(CHUNK, COPIED) };
			});
		},
	};
}

import { readdir } from "node:fs/promises";
import { join } from "node:path";
import type { BunPlugin } from "bun";

type SourceExport = string | { types?: string; default: string };

/** Resolves `@moq/<name>[/<sub>]` through the variant checkout's package exports. */
export function workspace(root: string): BunPlugin {
	return {
		name: "workspace",
		async setup(build) {
			const packages = new Map<string, { dir: string; exports: Record<string, SourceExport> }>();
			for (const entry of await readdir(join(root, "js"), { withFileTypes: true })) {
				if (!entry.isDirectory()) continue;
				const manifest = Bun.file(join(root, "js", entry.name, "package.json"));
				if (!(await manifest.exists())) continue;
				const { name, exports } = await manifest.json();
				if (typeof exports !== "object" || exports === null) continue;
				packages.set(name, { dir: join(root, "js", entry.name), exports });
			}
			build.onResolve({ filter: /^@moq\// }, (args) => {
				const [scope, name, ...rest] = args.path.split("/");
				const found = packages.get(`${scope}/${name}`);
				// Published dependencies resolve normally.
				if (!found) return undefined;
				const key = rest.length ? `./${rest.join("/")}` : ".";
				let target = found.exports[key];
				// Older baselines expose the same decoder through Container.Cmaf. Adapt the
				// benchmark import at build time, outside every measured phase.
				if (args.path === "@moq/hang/cmaf" && target === undefined) {
					const container = found.exports["./container"];
					if (typeof container !== "string") throw new Error(`missing CMAF decoder in ${found.dir}`);
					return { path: `${join(found.dir, container)}?bench-cmaf`, namespace: "legacy-cmaf" };
				}
				// Declaration metadata must not replace the runtime module. Refuse other
				// conditions rather than silently choose the wrong variant's implementation.
				if (
					target &&
					typeof target === "object" &&
					Object.keys(target).every((key) => key === "types" || key === "default")
				)
					target = target.default;
				if (typeof target !== "string") throw new Error(`${args.path} is not a source export of ${found.dir}`);
				return { path: join(found.dir, target) };
			});
			build.onResolve({ filter: /.*/, namespace: "legacy-cmaf" }, (args) => ({
				path: args.path,
				namespace: "file",
			}));
			build.onLoad({ filter: /.*/, namespace: "legacy-cmaf" }, (args) => ({
				loader: "js",
				contents: `import { Cmaf } from ${JSON.stringify(args.path.slice(0, -"?bench-cmaf".length))};
export const { decodeInitSegment, decodeDataSegment } = Cmaf;`,
			}));
		},
	};
}

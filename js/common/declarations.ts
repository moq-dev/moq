// Checks a package's emitted `.d.ts` files for imports the target file no longer exports.
//
// `stripInternal` drops an `@internal` export from its own `.d.ts` but leaves the import in any
// file that names the type, so a published consumer sees a module that does not export it. Only
// declaration emit shows this, which is why it runs over `dist/` after the build rather than as a
// test that would have to run the compiler again.

import { dirname, join, relative, resolve } from "node:path";

type FileExports = {
	names: Set<string>;
	stars: string[];
	imports: Array<{ specifier: string; names: string[] }>;
};

/** Every import in `files` (path to `.d.ts` source) that its relative target does not export. */
export function problems(root: string, files: Map<string, string>): string[] {
	const parsed = new Map<string, FileExports>();
	for (const [file, source] of files) {
		parsed.set(file, fileExports(source));
	}

	const found: string[] = [];
	for (const [file, info] of parsed) {
		for (const { specifier, names } of info.imports) {
			const target = dtsPath(parsed, file, specifier);
			if (!target) continue;

			if (!parsed.has(target)) {
				found.push(`${relative(root, file)}: imports ${specifier}, which emitted no declarations`);
				continue;
			}

			for (const name of names) {
				if (!exported(parsed, target, name)) {
					found.push(`${relative(root, file)}: imports ${name} from ${specifier}, which does not export it`);
				}
			}
		}
	}
	return found;
}

// The declaration file a relative specifier resolves to, as a file or a directory index. A package
// specifier resolves outside the package, so it is not checked.
function dtsPath(files: Map<string, unknown>, from: string, specifier: string): string | undefined {
	if (!specifier.startsWith(".")) return undefined;
	const base = resolve(dirname(from), specifier).replace(/\.(js|jsx|ts|tsx)$/, "");
	const index = join(base, "index.d.ts");
	return files.has(index) ? index : `${base}.d.ts`;
}

function exported(files: Map<string, FileExports>, file: string, name: string, seen = new Set<string>()): boolean {
	if (seen.has(file)) return false;
	seen.add(file);

	const info = files.get(file);
	if (!info) return false;
	if (info.names.has(name)) return true;

	for (const specifier of info.stars) {
		const target = dtsPath(files, file, specifier);
		if (target && exported(files, target, name, seen)) return true;
	}
	return false;
}

function fileExports(source: string): FileExports {
	const names = new Set<string>();
	const stars: string[] = [];
	const imports: Array<{ specifier: string; names: string[] }> = [];

	const named = /^(import|export)(?:\s+type)?\s*\{([\s\S]*?)\}\s*(?:from\s+["']([^"']+)["'])?/gm;
	const star = /^export\s+\*\s+from\s+["']([^"']+)["']/gm;
	const asStar = /^export\s+\*\s+as\s+([A-Za-z_$][\w$]*)\s+from\s+["']([^"']+)["']/gm;
	const decl =
		/^export\s+(?:declare\s+)?(?:abstract\s+)?(?:type|interface|class|function|const|enum|namespace)\s+([A-Za-z_$][\w$]*)/gm;

	for (const match of source.matchAll(named)) {
		const [, kind, list, specifier] = match;
		if (specifier) imports.push({ specifier, names: ident(list ?? "", "imported") });
		if (kind === "export") {
			for (const name of ident(list ?? "", "exported")) names.add(name);
		}
	}

	for (const match of source.matchAll(star)) {
		if (match[1]) stars.push(match[1]);
	}

	for (const match of source.matchAll(asStar)) {
		if (match[1]) names.add(match[1]);
	}

	for (const match of source.matchAll(decl)) {
		if (match[1]) names.add(match[1]);
	}

	if (/^export\s+default\b/m.test(source)) names.add("default");

	return { names, stars, imports };
}

function ident(list: string, side: "imported" | "exported"): string[] {
	return list
		.split(",")
		.map((part) => part.trim())
		.filter(Boolean)
		.map((part) => {
			const [left, right] = part.replace(/^type\s+/, "").split(/\s+as\s+/);
			const name = side === "imported" ? left : (right ?? left);
			return name?.trim() ?? "";
		})
		.filter(Boolean);
}

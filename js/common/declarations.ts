// Checks a package's emitted `.d.ts` files for imports the target file no longer exports.
//
// `stripInternal` drops an `@internal` export from its own `.d.ts` but leaves the import in any
// file that names the type, so a published consumer sees a module that does not export it. Only
// declaration emit shows this, which is why it runs over `dist/` after the build rather than as a
// test that would have to run the compiler again.

import { dirname, extname, join, relative, resolve } from "node:path";
import { parse } from "@babel/parser";

type Statement = ReturnType<typeof parse>["program"]["body"][number];
type Declaration = Extract<Statement, { type: "ExportNamedDeclaration" }>["declaration"];

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
// specifier resolves outside the package, and an asset (`./icon.svg?raw`) is typed by the bundler,
// so neither is checked.
function dtsPath(files: Map<string, unknown>, from: string, specifier: string): string | undefined {
	if (!specifier.startsWith(".")) return undefined;
	const ext = extname(specifier);
	if (ext && !/^\.(js|jsx|ts|tsx)$/.test(ext)) return undefined;
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

	const ast = parse(source, { sourceType: "module", plugins: [["typescript", { dts: true }]] });
	for (const statement of ast.program.body) {
		switch (statement.type) {
			case "ImportDeclaration":
				if (statement.specifiers.length === 0) break;
				imports.push({
					specifier: statement.source.value,
					names: statement.specifiers.flatMap((s) => {
						if (s.type === "ImportDefaultSpecifier") return ["default"];
						if (s.type === "ImportSpecifier") return [moduleName(s.imported)];
						return [];
					}),
				});
				break;
			case "ExportNamedDeclaration": {
				const imported: string[] = [];
				for (const s of statement.specifiers) {
					names.add(moduleName(s.exported));
					if (s.type === "ExportSpecifier") imported.push(moduleName(s.local));
				}
				if (statement.source) imports.push({ specifier: statement.source.value, names: imported });
				for (const name of declared(statement.declaration)) names.add(name);
				break;
			}
			case "ExportAllDeclaration":
				stars.push(statement.source.value);
				break;
			case "ExportDefaultDeclaration":
				names.add("default");
				break;
		}
	}

	return { names, stars, imports };
}

function moduleName(node: { type: "Identifier"; name: string } | { type: "StringLiteral"; value: string }): string {
	return node.type === "Identifier" ? node.name : node.value;
}

// The names an `export declare ...` statement introduces.
function declared(declaration: Declaration | null | undefined): string[] {
	if (!declaration) return [];
	if (declaration.type === "VariableDeclaration") {
		return declaration.declarations.flatMap((d) => (d.id.type === "Identifier" ? [d.id.name] : []));
	}
	if (!("id" in declaration) || !declaration.id) return [];
	return declaration.id.type === "Identifier" ? [declaration.id.name] : [];
}

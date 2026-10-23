// Checks a package's emitted `.d.ts` files for names that no longer resolve.
//
// `stripInternal` drops an `@internal` declaration but leaves every reference to it: an import in
// another file, which then names an export its target lacks, or a type in the same file, which
// then names nothing. A published consumer sees a broken type either way. Only declaration emit
// shows this, which is why it runs over `dist/` after the build rather than as a test that would
// have to run the compiler again.

import { dirname, extname, join, relative, resolve } from "node:path";
import { type ParserPlugin, parse } from "@babel/parser";

type Statement = ReturnType<typeof parse>["program"]["body"][number];
type Declaration = Extract<Statement, { type: "ExportNamedDeclaration" }>["declaration"];
type DefaultDeclaration = Extract<Statement, { type: "ExportDefaultDeclaration" }>["declaration"];

type FileExports = {
	names: Set<string>;
	stars: string[];
	imports: Array<{ specifier: string; names: string[] }>;
	// Every module-scope name, exported or not, including imports.
	bound: Set<string>;
	// Every name the file looks up at module scope.
	referenced: Set<string>;
};

/** The TypeScript a declaration file was emitted from. */
export type Source = { path: string; text: string };

/**
 * Every import in `files` (path to `.d.ts` source) that its relative target does not export, and
 * every name a file references without declaring that its source in `sources` (keyed like `files`)
 * declares at module scope.
 *
 * A name its source does not declare either is a global, so only the source can tell a stripped
 * declaration from one the declarations rightly leave to the environment.
 */
export function problems(root: string, files: Map<string, string>, sources = new Map<string, Source>()): string[] {
	const parsed = new Map<string, FileExports>();
	for (const [file, source] of files) {
		parsed.set(file, fileExports(source));
	}

	const found: string[] = [];
	for (const [file, info] of parsed) {
		const source = sources.get(file);
		if (source) {
			const plugins: ParserPlugin[] = extname(source.path) === ".tsx" ? ["typescript", "jsx"] : ["typescript"];
			const declaredBySource = bindings(parse(source.text, { sourceType: "module", plugins }).program.body);
			for (const name of info.referenced) {
				if (declaredBySource.has(name) && !info.bound.has(name)) {
					found.push(`${relative(root, file)}: references ${name}, which its declarations do not declare`);
				}
			}
		}

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

/**
 * The declaration file a relative specifier resolves to: the file itself, else a directory index.
 *
 * Undefined for a package specifier, which resolves outside the package, and for an asset
 * (`./icon.svg?raw`), which the bundler types.
 */
export function dtsPath(files: Map<string, unknown>, from: string, specifier: string): string | undefined {
	if (!specifier.startsWith(".")) return undefined;
	const ext = extname(specifier);
	if (ext && !/^\.(js|jsx|ts|tsx)$/.test(ext)) return undefined;
	const base = resolve(dirname(from), specifier).replace(/\.(js|jsx|ts|tsx)$/, "");
	const file = `${base}.d.ts`;
	const index = join(base, "index.d.ts");
	// A trailing slash names the directory, so only its index can match.
	if (specifier.endsWith("/")) return index;
	return files.has(file) || !files.has(index) ? file : index;
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

	return { names, stars, imports, bound: bindings(ast.program.body), referenced: references(ast.program.body) };
}

// The names a module declares or imports at its top level.
function bindings(body: Statement[]): Set<string> {
	const names = new Set<string>();
	for (const statement of body) {
		switch (statement.type) {
			case "ImportDeclaration":
				for (const s of statement.specifiers) names.add(s.local.name);
				break;
			case "TSImportEqualsDeclaration":
				names.add(statement.id.name);
				break;
			case "ExportNamedDeclaration":
			case "ExportDefaultDeclaration":
				for (const name of declared(statement.declaration)) names.add(name);
				break;
			default:
				for (const name of declared(statement)) names.add(name);
		}
	}
	return names;
}

// The leftmost identifier of every name the AST looks up in its enclosing scope: type references,
// `typeof` queries, heritage clauses, computed keys, and local re-exports. Type parameters are
// excluded file-wide, so a stripped name that shares a type parameter's name goes unnoticed.
function references(body: Statement[]): Set<string> {
	const found = new Set<string>();
	const parameters = new Set<string>();

	const lookup = (node: unknown, into = found) => {
		const name = leftmost(node);
		if (name) into.add(name);
	};

	const walk = (node: unknown) => {
		if (Array.isArray(node)) {
			for (const child of node) walk(child);
			return;
		}
		if (!node || typeof node !== "object" || !("type" in node)) return;
		const n = node as Record<string, unknown>;

		switch (n.type) {
			case "TSTypeReference":
				lookup(n.typeName);
				break;
			case "TSTypeQuery":
				lookup(n.exprName);
				break;
			case "TSInterfaceHeritage":
			case "TSClassImplements":
				lookup(n.expression);
				break;
			case "ClassDeclaration":
				lookup(n.superClass);
				break;
			case "ExportNamedDeclaration":
				if (!n.source) for (const s of n.specifiers as Array<{ local: unknown }>) lookup(s.local);
				break;
			case "ExportDefaultDeclaration":
				lookup(n.declaration);
				break;
			case "TSTypeParameter":
				lookup(n.name, parameters);
				break;
			case "TSMappedType":
				lookup(n.key, parameters);
				break;
		}
		if (n.computed) lookup(n.key);

		for (const [key, value] of Object.entries(n)) {
			if (key === "loc" || key.endsWith("Comments")) continue;
			walk(value);
		}
	};
	walk(body);

	for (const name of parameters) found.delete(name);
	return found;
}

// The identifier a qualified name, member expression, or plain identifier starts with.
function leftmost(node: unknown): string | undefined {
	if (!node || typeof node !== "object" || !("type" in node)) return undefined;
	const n = node as Record<string, unknown>;
	if (n.type === "Identifier") return n.name as string;
	if (n.type === "TSQualifiedName") return leftmost(n.left);
	if (n.type === "MemberExpression") return leftmost(n.object);
	return undefined;
}

/** The name an import or export specifier spells, as an identifier or a string. */
export function moduleName(
	node: { type: "Identifier"; name: string } | { type: "StringLiteral"; value: string },
): string {
	return node.type === "Identifier" ? node.name : node.value;
}

// The names a declaration statement introduces.
function declared(declaration: Statement | Declaration | DefaultDeclaration | null | undefined): string[] {
	if (!declaration) return [];
	if (declaration.type === "VariableDeclaration") {
		return declaration.declarations.flatMap((d) => (d.id.type === "Identifier" ? [d.id.name] : []));
	}
	if (!("id" in declaration) || !declaration.id) return [];
	return declaration.id.type === "Identifier" ? [declaration.id.name] : [];
}

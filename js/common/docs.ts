// Checks that a package's public surface carries doc comments.
//
// It walks the emitted `.d.ts` files from each entrypoint through their re-export chains, so it
// sees what a consumer can import rather than every `export` in a file, and `stripInternal` has
// already dropped the `@internal` symbols. TypeScript 7 has no stable compiler API and TypeDoc
// does not support it, so this parses declarations the same way `declarations.ts` does.

import { dirname, extname, join, relative, resolve } from "node:path";
import { parse } from "@babel/parser";

type Program = ReturnType<typeof parse>["program"];
type Node = Program["body"][number];
type Comment = { type: string; value: string };

type Binding =
	| { kind: "local"; name: string }
	| { kind: "import"; specifier: string; name: string }
	| { kind: "namespace"; specifier: string };

type Module = {
	// Top-level declarations by name, each with the statement that carries its comments.
	locals: Map<string, Array<{ statement: Node; declaration: Node }>>;
	imports: Map<string, Binding>;
	exports: Map<string, Binding>;
	stars: string[];
};

/** Every exported symbol, or public member of an exported class, reachable from `entrypoints` without a doc comment. */
export function undocumented(root: string, files: Map<string, string>, entrypoints: string[]): string[] {
	const modules = new Map<string, Module>();
	const load = (file: string): Module => {
		let module = modules.get(file);
		if (!module) {
			const source = files.get(file);
			if (source === undefined) throw new Error(`${relative(root, file)}: no such declaration file`);
			module = parseModule(source);
			modules.set(file, module);
		}
		return module;
	};

	const found: string[] = [];
	const seen = new Set<string>();

	// Report each declaration once, under the first public name that reaches it.
	const visit = (file: string, path: string, binding: Binding) => {
		const resolved = resolveBinding(file, binding);
		if (!resolved) return;
		const key = `${resolved.file}#${resolved.kind === "namespace" ? "*" : resolved.name}`;
		if (seen.has(key)) return;
		seen.add(key);

		if (resolved.kind === "namespace") {
			walk(resolved.file, `${path}.`);
			return;
		}

		const declarations = load(resolved.file).locals.get(resolved.name);
		if (!declarations) throw new Error(`${relative(root, resolved.file)}: cannot find ${resolved.name}`);
		const where = relative(root, resolved.file);
		if (!declarations.some(({ statement }) => documented(statement))) found.push(`${where}: ${path}`);

		const members = new Map<string, boolean>();
		for (const { declaration } of declarations) {
			if (declaration.type !== "ClassDeclaration") continue;
			for (const member of declaration.body.body) {
				const name = memberName(member);
				if (name === undefined) continue;
				members.set(name, (members.get(name) ?? false) || documented(member));
			}
		}
		for (const [name, ok] of members) {
			if (!ok) found.push(`${where}: ${path}.${name}`);
		}
	};

	// Follow a binding to the file and local name that declares it; undefined for another package.
	const resolveBinding = (
		file: string,
		binding: Binding,
		chain = new Set<string>(),
	): { kind: "local"; file: string; name: string } | { kind: "namespace"; file: string } | undefined => {
		if (binding.kind === "local") {
			const imported = load(file).imports.get(binding.name);
			if (imported) return resolveBinding(file, imported, chain);
			return { kind: "local", file, name: binding.name };
		}
		const target = dtsPath(files, file, binding.specifier);
		if (!target) return undefined;
		if (binding.kind === "namespace") return { kind: "namespace", file: target };
		const next = findExport(target, binding.name, chain);
		if (!next) throw new Error(`${relative(root, file)}: ${binding.specifier} does not export ${binding.name}`);
		return resolveBinding(next.file, next.binding, chain);
	};

	const findExport = (
		file: string,
		name: string,
		chain: Set<string>,
	): { file: string; binding: Binding } | undefined => {
		const key = `${file}#${name}`;
		if (chain.has(key)) return undefined;
		chain.add(key);

		const module = load(file);
		const binding = module.exports.get(name);
		if (binding) return { file, binding };
		for (const specifier of module.stars) {
			const target = dtsPath(files, file, specifier);
			if (!target) continue;
			const found = findExport(target, name, chain);
			if (found) return found;
		}
		return undefined;
	};

	const walk = (file: string, prefix: string, chain = new Set<string>()) => {
		if (chain.has(file)) return;
		chain.add(file);
		const module = load(file);
		for (const [name, binding] of module.exports) visit(file, prefix + name, binding);
		for (const specifier of module.stars) {
			const target = dtsPath(files, file, specifier);
			if (target) walk(target, prefix, chain);
		}
	};

	for (const entrypoint of entrypoints) walk(entrypoint, "");
	return found;
}

function parseModule(source: string): Module {
	const module: Module = { locals: new Map(), imports: new Map(), exports: new Map(), stars: [] };
	const declare = (name: string, statement: Node, declaration: Node) => {
		const list = module.locals.get(name) ?? [];
		list.push({ statement, declaration });
		module.locals.set(name, list);
	};

	const ast = parse(source, { sourceType: "module", plugins: [["typescript", { dts: true }], "jsx"] });
	for (const statement of ast.program.body) {
		switch (statement.type) {
			case "ImportDeclaration":
				for (const s of statement.specifiers) {
					const specifier = statement.source.value;
					if (s.type === "ImportNamespaceSpecifier") {
						module.imports.set(s.local.name, { kind: "namespace", specifier });
					} else {
						const name = s.type === "ImportDefaultSpecifier" ? "default" : moduleName(s.imported);
						module.imports.set(s.local.name, { kind: "import", specifier, name });
					}
				}
				break;
			case "ExportNamedDeclaration":
				for (const s of statement.specifiers) {
					const exported = moduleName(s.exported);
					if (s.type === "ExportNamespaceSpecifier") {
						if (statement.source)
							module.exports.set(exported, { kind: "namespace", specifier: statement.source.value });
					} else if (s.type === "ExportSpecifier") {
						const name = moduleName(s.local);
						module.exports.set(
							exported,
							statement.source
								? { kind: "import", specifier: statement.source.value, name }
								: { kind: "local", name },
						);
					}
				}
				for (const name of declared(statement.declaration)) {
					declare(name, statement, statement.declaration as Node);
					module.exports.set(name, { kind: "local", name });
				}
				break;
			case "ExportAllDeclaration":
				module.stars.push(statement.source.value);
				break;
			case "ExportDefaultDeclaration": {
				const declaration = statement.declaration;
				if (declaration.type === "Identifier") {
					module.exports.set("default", { kind: "local", name: declaration.name });
				} else {
					declare("default", statement, declaration as Node);
					module.exports.set("default", { kind: "local", name: "default" });
				}
				break;
			}
			default:
				for (const name of declared(statement)) declare(name, statement, statement);
		}
	}
	return module;
}

// A doc comment is a non-empty `/** */` block before the node.
function documented(node: { leadingComments?: Comment[] | null }): boolean {
	return (node.leadingComments ?? []).some((c) => c.type === "CommentBlock" && /^\*[\s*]*[^\s*/]/.test(c.value));
}

// Custom element reactions: the browser calls them, and the HTML spec documents them.
const REACTIONS = new Set([
	"observedAttributes",
	"connectedCallback",
	"disconnectedCallback",
	"adoptedCallback",
	"connectedMoveCallback",
	"attributeChangedCallback",
]);

// The name of a class member a consumer can use, or undefined for one that needs no doc:
// constructors, private and protected members, `#private`, custom element reactions, and
// symbol-keyed members such as brands.
function memberName(member: Node | { type: string; [key: string]: unknown }): string | undefined {
	const m = member as {
		type: string;
		kind?: string;
		computed?: boolean;
		accessibility?: string;
		key?: { type: string; name?: string; value?: unknown };
	};
	if (m.type === "ClassPrivateProperty" || m.type === "ClassPrivateMethod") return undefined;
	if (m.type === "TSIndexSignature" || m.type === "StaticBlock") return undefined;
	if (m.kind === "constructor") return undefined;
	if (m.accessibility === "private" || m.accessibility === "protected") return undefined;
	if (m.computed || !m.key) return undefined;
	let name: string | undefined;
	if (m.key.type === "Identifier") name = m.key.name;
	else if (m.key.type === "StringLiteral" || m.key.type === "NumericLiteral") name = String(m.key.value);
	return name === undefined || REACTIONS.has(name) ? undefined : name;
}

// The declaration file a relative specifier resolves to, as a file or a directory index. A package
// specifier resolves to another package, which checks its own surface, so it is not followed.
function dtsPath(files: Map<string, unknown>, from: string, specifier: string): string | undefined {
	if (!specifier.startsWith(".")) return undefined;
	const ext = extname(specifier);
	if (ext && !/^\.(js|jsx|ts|tsx)$/.test(ext)) return undefined;
	const base = resolve(dirname(from), specifier).replace(/\.(js|jsx|ts|tsx)$/, "");
	const index = join(base, "index.d.ts");
	return files.has(index) ? index : `${base}.d.ts`;
}

function moduleName(node: { type: "Identifier"; name: string } | { type: "StringLiteral"; value: string }): string {
	return node.type === "Identifier" ? node.name : node.value;
}

// The names a declaration statement introduces.
function declared(declaration: unknown): string[] {
	const d = declaration as
		| {
				type: string;
				id?: { type: string; name?: string } | null;
				declarations?: Array<{ id: { type: string; name?: string } }>;
		  }
		| null
		| undefined;
	if (!d) return [];
	if (d.type === "VariableDeclaration") {
		return (d.declarations ?? []).flatMap((v) => (v.id.type === "Identifier" && v.id.name ? [v.id.name] : []));
	}
	if (!/Declaration$|^TSDeclareFunction$/.test(d.type)) return [];
	return d.id?.type === "Identifier" && d.id.name ? [d.id.name] : [];
}

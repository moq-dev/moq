// Checks that a package's public surface carries doc comments.
//
// It walks the emitted `.d.ts` files from each entrypoint through their re-export chains, so it
// sees what a consumer can import rather than every `export` in a file, and `stripInternal` has
// already dropped the `@internal` symbols. TypeScript 7 has no stable compiler API and TypeDoc
// does not support it, so this parses declarations the same way `declarations.ts` does.

import { relative } from "node:path";
import { parse } from "@babel/parser";
import { dtsPath, moduleName } from "./declarations.ts";

type Program = ReturnType<typeof parse>["program"];
type Node = Program["body"][number];
type Comment = { type: string; value: string; end?: number };

type Binding =
	| { kind: "local"; name: string }
	| { kind: "import"; specifier: string; name: string }
	// `documented` is set for an `export * as Name`, whose own statement carries the doc, and
	// `types` for `export type * as Name`, which re-exports only types.
	| { kind: "namespace"; specifier: string; documented?: boolean; types?: boolean };

// A name's declarations, each with the statement that carries its comments.
type Declarations = Array<{ statement: Node; declaration: Node }>;

type Module = {
	// Top-level declarations by name.
	locals: Map<string, Declarations>;
	imports: Map<string, Binding>;
	exports: Map<string, Binding>;
	// `export *` targets; `types` marks `export type *`, which re-exports only types.
	stars: Array<{ specifier: string; types: boolean }>;
	// Where the first statement starts; a `@module` block above it documents the module.
	header: number;
	// Whether the file carries a `@module` block, which an entrypoint needs.
	described: boolean;
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
	const aliases = new Set<Binding>();

	// Report each declaration once, under the first public name that reaches it.
	// Under `types`, a name reached only through `export type *` that is purely a value is skipped.
	const visit = (file: string, path: string, binding: Binding, types: boolean) => {
		// The `export * as Name` statement is what a consumer sees, whichever package it names.
		if (binding.kind === "namespace" && binding.documented === false && !aliases.has(binding)) {
			aliases.add(binding);
			found.push(`${relative(root, file)}: ${path}`);
		}
		const resolved = resolveBinding(file, binding);
		if (!resolved) return;
		if (resolved.kind === "namespace") {
			// A namespace walked for its types alone is walked again where its values are public.
			const typesOnly = types || (binding.kind === "namespace" && binding.types === true);
			const key = `${resolved.file}#*${typesOnly ? ":types" : ""}`;
			if (seen.has(key)) return;
			seen.add(key);
			walk(resolved.file, `${path}.`, typesOnly);
			return;
		}

		const declarations = load(resolved.file).locals.get(resolved.name);
		if (!declarations) throw new Error(`${relative(root, resolved.file)}: cannot find ${resolved.name}`);
		if (types && declarations.every(({ declaration }) => VALUES.has(declaration.type))) return;
		const key = `${resolved.file}#${resolved.name}`;
		if (seen.has(key)) return;
		seen.add(key);
		check(resolved.file, path, key, declarations);
	};

	// Check one name's declarations, the public members of a class and of the package's classes it
	// extends, and everything a namespace declares, since an ambient namespace exports all of it.
	// `key` names the declaration by file and local path, so each is reported once.
	const check = (file: string, path: string, key: string, declarations: Declarations) => {
		const where = relative(root, file);
		const header = load(file).header;
		if (!declarations.some(({ statement }) => documented(statement, header))) found.push(`${where}: ${path}`);

		const members = new Map<string, boolean>();
		for (const { declaration } of declarations) {
			if (declaration.type === "ClassDeclaration") {
				const superClass = declaration.superClass;
				const base = resolveClass(file, superClass);
				if (base && "key" in base && !seen.has(base.key)) {
					seen.add(base.key);
					check(base.file, base.name, base.key, base.declarations);
				}
				const element = isElement(superClass);
				for (const member of declaration.body.body) {
					const name = memberName(member, element);
					if (name === undefined) continue;
					members.set(name, (members.get(name) ?? false) || documented(member, header));
				}
			}
		}
		for (const [name, ok] of members) {
			if (!ok) found.push(`${where}: ${path}.${name}`);
		}
		for (const [name, list] of namespaceMembers(declarations)) {
			const nested = `${key}.${name}`;
			if (seen.has(nested)) continue;
			seen.add(nested);
			check(file, `${path}.${name}`, nested, list);
		}
	};

	// What a superclass expression names: a module through a namespace import, or a package
	// declaration reached by name or through any depth of namespaces (`ns.Inner.Parent`).
	// Undefined for a global or another package's class.
	type Named = { key: string; file: string; name: string; declarations: Declarations };
	const resolveClass = (file: string, expression: unknown): Named | { module: string } | undefined => {
		const local = (base: ReturnType<typeof resolveBinding>): Named | { module: string } | undefined => {
			if (base?.kind === "namespace") return { module: base.file };
			const declarations = base ? load(base.file).locals.get(base.name) : undefined;
			if (!base || !declarations) return undefined;
			return { key: `${base.file}#${base.name}`, file: base.file, name: base.name, declarations };
		};
		const node = expression as { type: string; name?: string; object?: unknown; property?: unknown } | null;
		if (node?.type === "Identifier" && node.name)
			return local(resolveBinding(file, { kind: "local", name: node.name }));

		const property = node?.property as { type: string; name: string } | undefined;
		if (node?.type !== "MemberExpression" || property?.type !== "Identifier") return undefined;
		const owner = resolveClass(file, node.object);
		if (!owner) return undefined;
		if ("module" in owner) {
			const next = findExport(owner.module, property.name, new Set());
			return next ? local(resolveBinding(next.file, next.binding)) : undefined;
		}
		const declarations = namespaceMembers(owner.declarations).get(property.name);
		if (!declarations) return undefined;
		const name = `${owner.name}.${property.name}`;
		return { key: `${owner.file}#${name}`, file: owner.file, name, declarations };
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
		if (name === "default") return undefined;
		for (const { specifier } of module.stars) {
			const target = dtsPath(files, file, specifier);
			if (!target) continue;
			const found = findExport(target, name, chain);
			if (found) return found;
		}
		return undefined;
	};

	// `export *` never re-exports a default, nor a name the re-exporting module exports itself.
	// `chain` holds only the active path, so a module reached again in another context (a value
	// star after a type star, or under different shadowing) is walked again; `seen` stops repeats.
	const walk = (file: string, prefix: string, types = false, chain = new Set<string>(), hidden?: Set<string>) => {
		if (chain.has(file)) return;
		chain.add(file);
		const module = load(file);
		for (const [name, binding] of module.exports) {
			if (!hidden?.has(name)) visit(file, prefix + name, binding, types);
		}
		const shadowed = new Set([...(hidden ?? ["default"]), ...module.exports.keys()]);
		for (const star of module.stars) {
			const target = dtsPath(files, file, star.specifier);
			if (target) walk(target, prefix, types || star.types, chain, shadowed);
		}
		chain.delete(file);
	};

	for (const entrypoint of entrypoints) {
		if (!load(entrypoint).described) found.push(`${relative(root, entrypoint)}: no @module block`);
		walk(entrypoint, "");
	}
	return found;
}

function parseModule(source: string): Module {
	const module: Module = {
		locals: new Map(),
		imports: new Map(),
		exports: new Map(),
		stars: [],
		header: 0,
		described: false,
	};
	const declare = (name: string, statement: Node, declaration: Node) => {
		const list = module.locals.get(name) ?? [];
		list.push({ statement, declaration });
		module.locals.set(name, list);
	};

	const ast = parse(source, { sourceType: "module", plugins: [["typescript", { dts: true }], "jsx"] });
	module.header = ast.program.body[0]?.start ?? Number.POSITIVE_INFINITY;
	module.described = (ast.comments ?? []).some((c) => isModuleBlock(c, module.header));
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
							module.exports.set(exported, {
								kind: "namespace",
								specifier: statement.source.value,
								documented: documented(statement, module.header),
								types: statement.exportKind === "type",
							});
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
				module.stars.push({ specifier: statement.source.value, types: statement.exportKind === "type" });
				break;
			case "ExportDefaultDeclaration": {
				const declaration = statement.declaration;
				// A named default declaration also binds its name, which an `export { Name }` can use.
				const id = (declaration as { id?: { type: string; name: string } | null }).id;
				const name =
					declaration.type === "Identifier"
						? declaration.name
						: id?.type === "Identifier"
							? id.name
							: "default";
				if (declaration.type !== "Identifier") declare(name, statement, declaration as Node);
				module.exports.set("default", { kind: "local", name });
				break;
			}
			default:
				for (const name of declared(statement)) declare(name, statement, statement);
		}
	}
	return module;
}

// Everything an ambient namespace among `declarations` declares, by name; all of it is exported.
function namespaceMembers(declarations: Declarations): Map<string, Declarations> {
	const members = new Map<string, Declarations>();
	for (const { statement: outer, declaration } of declarations) {
		if (declaration.type !== "TSModuleDeclaration" || declaration.body.type !== "TSModuleBlock") continue;
		// `namespace Outer.Inner { ... }` declares `Inner` inside `Outer`, with the same body.
		const names = path(declaration.id);
		if (names.length > 1) {
			const inner = { ...declaration, id: qualified(names.slice(1)) } as Node;
			const list = members.get(names[1]) ?? [];
			list.push({ statement: outer, declaration: inner });
			members.set(names[1], list);
			continue;
		}
		for (const statement of declaration.body.body) {
			const inner = statement.type === "ExportNamedDeclaration" ? statement.declaration : statement;
			for (const name of declared(inner)) {
				const list = members.get(name) ?? [];
				list.push({ statement, declaration: inner as Node });
				members.set(name, list);
			}
		}
	}
	return members;
}

// A `@module` doc block above a file's first statement, which documents the module.
function isModuleBlock(c: Comment, header: number): boolean {
	return c.type === "CommentBlock" && (c.end ?? 0) <= header && /^\*[\s\S]*@module\b/.test(c.value);
}

// A doc comment is a non-empty `/** */` block before the node, other than the file's module block.
function documented(node: { leadingComments?: Comment[] | null }, header: number): boolean {
	return (node.leadingComments ?? []).some(
		(c) => c.type === "CommentBlock" && /^\*[\s*]*[^\s*/]/.test(c.value) && !isModuleBlock(c, header),
	);
}

// Declarations that are only values, which `export type *` does not re-export.
const VALUES = new Set(["VariableDeclaration", "TSDeclareFunction", "FunctionDeclaration"]);

// Custom element reactions: on an element, the browser calls them and the HTML spec documents them.
const REACTIONS = new Set([
	"observedAttributes",
	"connectedCallback",
	"disconnectedCallback",
	"adoptedCallback",
	"connectedMoveCallback",
	"attributeChangedCallback",
]);

// Whether a class extends an element class such as `HTMLElement`, by its name.
function isElement(superClass: unknown): boolean {
	const node = superClass as { type: string; name?: string; property?: { name?: string } } | null | undefined;
	const name = node?.type === "MemberExpression" ? node.property?.name : node?.name;
	return name !== undefined && /Element$/.test(name);
}

// The name of a class member a consumer can use, or undefined for one that needs no doc:
// constructors, private and protected members, `#private`, an element's custom element
// reactions, and members keyed by a symbol other than a well-known one, such as brands.
function memberName(member: unknown, element: boolean): string | undefined {
	const m = member as {
		type: string;
		kind?: string;
		computed?: boolean;
		accessibility?: string;
		key?: {
			type: string;
			name?: string;
			value?: unknown;
			object?: { type: string; name?: string };
			property?: { type: string; name?: string };
		};
	};
	if (m.type === "ClassPrivateProperty" || m.type === "ClassPrivateMethod") return undefined;
	if (m.type === "TSIndexSignature" || m.type === "StaticBlock") return undefined;
	if (m.kind === "constructor") return undefined;
	if (m.accessibility === "private" || m.accessibility === "protected") return undefined;
	if (!m.key) return undefined;
	let name: string | undefined;
	if (m.key.type === "Identifier" && !m.computed) name = m.key.name;
	else if (m.key.type === "StringLiteral" || m.key.type === "NumericLiteral") name = String(m.key.value);
	// A well-known symbol (`[Symbol.dispose]`) is a protocol a consumer uses, unlike a private brand.
	else if (m.key.type === "MemberExpression" && m.key.object?.name === "Symbol" && m.key.property?.name) {
		name = `[Symbol.${m.key.property.name}]`;
	}
	return name === undefined || (element && REACTIONS.has(name)) ? undefined : name;
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
	if (d.type === "TSModuleDeclaration") return path(d.id).slice(0, 1);
	return d.id?.type === "Identifier" && d.id.name ? [d.id.name] : [];
}

// The names of a dotted namespace id (`Outer.Inner`), outermost first; empty for a string id.
function path(id: unknown): string[] {
	const node = id as { type: string; name?: string; left?: unknown; right?: unknown } | null | undefined;
	if (node?.type === "Identifier" && node.name) return [node.name];
	if (node?.type === "TSQualifiedName") return [...path(node.left), ...path(node.right)];
	return [];
}

// A dotted namespace id from its names, the inverse of {@link path}.
function qualified(names: string[]): unknown {
	const [first, ...rest] = names;
	return rest.reduce<unknown>(
		(left, name) => ({ type: "TSQualifiedName", left, right: { type: "Identifier", name } }),
		{ type: "Identifier", name: first },
	);
}

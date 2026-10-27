// The public API report every `@moq/*` package commits: one `api/*.api.md` per `exports`
// entrypoint, rendered from TypeDoc's model of the built declarations, and diffed against the last
// release by `sh/api-diff`.
//
// The declarations are read from a copy outside the repository whose `node_modules` holds only the
// dependencies its manifest declares, as a consumer's install would, so an undeclared import or a
// broken declaration fails the check. A workspace dependency resolves to its source, so a package's
// report never depends on whether its dependencies are built.
//
// `bun js/common/api.ts check <dir>...` checks built packages, as `just check` and `just fix` run it
// after their builds. A package's own build does not, so a consumer building one from source needs
// none of this toolchain. TypeDoc comes from moq-doc-api, which pins the TypeScript 6 it needs.
//
// `bun js/common/api.ts baseline <checkout> <out>` renders every published package of another
// checkout, such as the release branch, with this renderer, so `sh/api-diff` compares like with like.

import {
	cpSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	realpathSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import {
	Application,
	type Context,
	Converter,
	type DeclarationReflection,
	type ParameterReflection,
	ProjectReflection,
	ReferenceReflection,
	ReferenceType,
	type Reflection,
	ReflectionKind,
	type ReflectionSymbolId,
	type SignatureReflection,
	type SomeType,
	type TypeParameterReflection,
} from "typedoc";

/**
 * Compare the built package in `dir` with its committed reports in `api/`, returning the stale
 * ones, or rewrite them when `MOQ_API_REPORT=update`, as `just fix` sets.
 */
export async function checkReports(dir: string): Promise<string[]> {
	const folder = join(dir, "api");
	const current = await reports(resolve(dir), join(resolve(dir), "dist"), true);
	const committed = existsSync(folder) ? readdirSync(folder).filter((file) => file.endsWith(".api.md")) : [];
	const stale = [...new Set([...current.keys(), ...committed])].filter((file) => {
		const path = join(folder, file);
		return current.get(file) !== (existsSync(path) ? readFileSync(path, "utf8") : undefined);
	});

	if (process.env.MOQ_API_REPORT !== "update") return stale.map((file) => join(folder, file));
	mkdirSync(folder, { recursive: true });
	for (const file of stale) {
		const report = current.get(file);
		if (report === undefined) rmSync(join(folder, file));
		else writeFileSync(join(folder, file), report);
	}
	return [];
}

interface Manifest {
	name: string;
	exports?: Record<string, string | { types?: string }>;
	scripts?: Record<string, string>;
	dependencies?: Record<string, string>;
	peerDependencies?: Record<string, string>;
	optionalDependencies?: Record<string, string>;
}

/**
 * Every entrypoint's report for the package in `dir`, by file name such as `index.api.md`, from
 * the declarations and manifest in `declarations`. `check` fails on a compiler error in them or a
 * reference to a declaration of this package that no entrypoint exports.
 */
async function reports(dir: string, declarations: string, check: boolean): Promise<Map<string, string>> {
	const work = mkdtempSync(join(tmpdir(), "moq-api-"));
	try {
		const root = join(realpathSync(work), "package");
		cpSync(declarations, root, { recursive: true });
		const manifest: Manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
		linkDependencies(dir, root, manifest);

		const entries = new Map<string, string>();
		for (const [subpath, target] of Object.entries(manifest.exports ?? {})) {
			const types = typeof target === "string" ? undefined : target.types;
			if (types) entries.set(join(root, types), subpath);
		}
		if (entries.size === 0) return new Map();

		writeFileSync(
			join(root, "tsconfig.json"),
			JSON.stringify({
				compilerOptions: {
					target: "esnext",
					module: "esnext",
					moduleResolution: "bundler",
					lib: ["esnext", "dom"],
					strict: true,
					types: [],
				},
				files: [...entries.keys()],
			}),
		);

		const app = await Application.bootstrap({
			entryPoints: [...entries.keys()],
			entryPointStrategy: "resolve",
			tsconfig: join(root, "tsconfig.json"),
			alwaysCreateEntryPointModule: true,
			readme: "none",
			excludePrivate: true,
			// The report checks its own compiler errors, in this package's files alone.
			skipErrorChecking: true,
			validation: { notExported: false, invalidLink: false, invalidPath: false, rewrittenLink: false },
			// Comments are not part of the API; the reference checks them.
			logLevel: "Error",
			// No limit: TypeDoc otherwise cuts a type nested past 10 levels to `...`, which would hide a
			// change in it. The option is parsed as an integer, so `Infinity` would read as 0.
			maxTypeConversionDepth: Number.MAX_SAFE_INTEGER,
		});
		const own = (file: string) => !relative(root, file).startsWith("..");
		const errors: string[] = [];
		if (check) {
			app.converter.on(Converter.EVENT_BEGIN, (context) => {
				for (const program of context.programs) {
					for (const file of program.getSourceFiles()) {
						if (!own(file.fileName)) continue;
						for (const diagnostic of [
							...program.getSyntacticDiagnostics(file),
							...program.getSemanticDiagnostics(file),
						]) {
							const at = diagnostic.start === undefined ? 0 : diagnostic.start;
							const { line } = file.getLineAndCharacterOfPosition(at);
							const text = ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n");
							errors.push(`${relative(root, file.fileName)}:${line + 1}: ${text}`);
						}
					}
				}
			});
		}
		// TypeDoc keeps no record of whether a variable is `const`, so note it from the declaration.
		const constants = new Set<Reflection>();
		app.converter.on(Converter.EVENT_CREATE_DECLARATION, (context, reflection) => {
			const symbol = context.getSymbolFromReflection(reflection);
			const declaration = symbol?.valueDeclaration;
			if (!declaration || !reflection.kindOf(ReflectionKind.Variable | ReflectionKind.Property)) return;
			if (ts.getCombinedNodeFlags(declaration) & ts.NodeFlags.Const) constants.add(reflection);
			const written = (declaration as { type?: TypeNode }).type;
			if (!written) return;
			reflection.type = context.converter.convertType(
				context.withScope(reflection),
				literal(written) ? written : context.checker.getTypeOfSymbolAtLocation(symbol, declaration),
			);
		});
		// A type-only export of a name that is both a value and a type exports the type, but TypeDoc
		// also converts the value, as an alias of its type: drop that one when the type follows.
		const values = new Map<TsSymbol, DeclarationReflection>();
		app.converter.on(Converter.EVENT_CREATE_DECLARATION, (context, reflection) => {
			if (!reflection.kindOf(ReflectionKind.TypeAlias | ReflectionKind.Interface)) return;
			const symbol = context.getSymbolFromReflection(reflection);
			if (!symbol || !(symbol.flags & ts.SymbolFlags.Value)) return;
			const value = values.get(symbol);
			if (value?.parent === reflection.parent) context.project.removeReflection(value);
			else if (reflection.kindOf(ReflectionKind.TypeAlias)) values.set(symbol, reflection);
		});
		// TypeDoc converts a property's or signature's types from the checker, which restates what the
		// declaration wrote, such as `[...S]` as `[...S[]]` and `keyof S` as its expansion, so a change
		// could hide behind the same restatement. The written type is the API, unless it names a value
		// with `typeof`: that value may be one no entrypoint exports, which the restatement spells out.
		// TypeDoc also drops the arguments and `typeof` of a written `import()` type.
		app.converter.on(Converter.EVENT_CREATE_SIGNATURE, (context, signature, declaration) => {
			if (!declaration || !("parameters" in declaration)) return;
			const scope = context.withScope(signature);
			for (const [i, parameter] of (signature.parameters ?? []).entries()) {
				const written = declaration.parameters[i]?.type;
				if (written && written.kind !== ts.SyntaxKind.ThisType && literal(written)) {
					parameter.type = context.converter.convertType(scope.withScope(parameter), written);
				}
			}
			const written = declaration.type;
			const derived = signature.type?.type === "predicate" || signature.kindOf(ReflectionKind.SetSignature);
			if (written && !derived && written.kind !== ts.SyntaxKind.ThisType && literal(written)) {
				signature.type = context.converter.convertType(scope, written);
			}
		});
		// Where the declared dependencies export each of their declarations, which names a reference
		// to one as a consumer would: `Audio.Decoder` from `@moq/watch`, not a bare `Decoder`. The
		// walk is breadth-first, so the shortest path is found first.
		const exported = new Map<string, Location>();
		app.converter.on(Converter.EVENT_END, (context) => {
			const [program] = context.programs;
			if (!program) return;
			context.setActiveProgram(program);
			const checker = program.getTypeChecker();
			// TypeDoc converts a `@reexport` alias as the declaration it names, so a type written with
			// the alias's name would resolve to nothing: register the alias for that reflection too.
			for (const source of program.getSourceFiles()) {
				if (!own(source.fileName)) continue;
				for (const statement of source.statements) {
					const alias = statement as { name?: TypeNode; type?: TypeNode & { typeName?: TypeNode } };
					const named = alias.type?.typeName;
					if (statement.kind !== ts.SyntaxKind.TypeAliasDeclaration || !alias.name || !named) continue;
					const symbol = checker.getSymbolAtLocation(alias.name);
					if (!symbol || !context.getComment(symbol, ReflectionKind.TypeAlias)?.hasModifier("@reexport"))
						continue;
					const type = checker.getTypeAtLocation(named);
					const target = type.aliasSymbol ?? type.getSymbol();
					const reflection =
						target && context.project.getReflectionFromSymbolId(context.createSymbolId(target));
					if (reflection) context.project.registerSymbolId(reflection, context.createSymbolId(symbol));
				}
			}
			const queue: { symbol: TsSymbol; specifier: string; path: string[] }[] = [];
			const seen = new Set<TsSymbol>();
			for (const dependency of dependencies(manifest)) {
				for (const specifier of entrypoints(dir, dependency)) {
					const resolved = ts.resolveModuleName(
						specifier,
						join(root, "package.json"),
						program.getCompilerOptions(),
						ts.sys,
					).resolvedModule;
					const source = resolved && program.getSourceFile(resolved.resolvedFileName);
					const symbol = source && checker.getSymbolAtLocation(source);
					if (symbol && !seen.has(symbol)) {
						seen.add(symbol);
						queue.push({ symbol, specifier, path: [] });
					}
				}
			}
			for (const { symbol, specifier, path } of queue) {
				for (const child of checker.getExportsOfModule(symbol)) {
					const target = context.resolveAliasedSymbol(child);
					const at = [...path, child.name];
					const id = key(context.createSymbolId(target));
					if (!exported.has(id)) exported.set(id, { specifier, path: at.join(".") });
					if (target.flags & ts.SymbolFlags.Module && !seen.has(target)) {
						seen.add(target);
						queue.push({ symbol: target, specifier, path: at });
					}
				}
			}
			context.setActiveProgram(undefined);
		});
		const project = await app.convert();
		if (!project || app.logger.hasErrors()) {
			throw new Error(`TypeDoc failed on ${manifest.name}`);
		}
		if (check) errors.push(...unexported(app, project, manifest.name));
		if (errors.length > 0) {
			throw new Error(`The declarations of ${manifest.name} are not consumable:\n  ${errors.join("\n  ")}`);
		}

		const modules = new Map<Reflection, string>();
		for (const module of project.children ?? []) {
			const file = module.sources?.[0]?.fullFileName;
			const subpath = file && entries.get(file);
			if (subpath === undefined) throw new Error(`${module.name} is not an entrypoint of ${manifest.name}`);
			modules.set(module, subpath === "." ? manifest.name : `${manifest.name}/${subpath.replace(/^\.\//, "")}`);
		}

		locate(project, modules, exported);
		const renderer = new Renderer({ project, name: manifest.name, modules, constants, exported, dir });
		const found = new Map<string, string>();
		for (const [module, specifier] of modules) {
			const subpath = specifier.slice(manifest.name.length + 1) || "index";
			found.set(`${subpath.replaceAll("/", "-")}.api.md`, renderer.report(module as DeclarationReflection));
		}
		return found;
	} finally {
		rmSync(work, { recursive: true, force: true });
	}
}

type Program = Context["programs"][number];
type TsSymbol = ReturnType<Context["resolveAliasedSymbol"]>;
// \`convertType\`'s last overload takes the type and the node it was written as.
type TypeNode = NonNullable<Parameters<Converter["convertType"]>[2]>;

// The TypeScript TypeDoc compiles with, for its diagnostics, node flags, and module resolution.
const ts: {
	flattenDiagnosticMessageText(text: unknown, newline: string): string;
	getCombinedNodeFlags(node: unknown): number;
	resolveModuleName(
		name: string,
		containingFile: string,
		options: ReturnType<Program["getCompilerOptions"]>,
		host: unknown,
	): { resolvedModule?: { resolvedFileName: string } };
	sys: unknown;
	NodeFlags: { Const: number };
	SymbolFlags: { Module: number; Value: number };
	SyntaxKind: { ThisType: number; TypeQuery: number; ImportType: number; TypeAliasDeclaration: number };
	forEachChild<T>(node: TypeNode, visit: (child: TypeNode) => T | undefined): T | undefined;
} = createRequire(import.meta.resolve("typedoc"))("typescript");

// Whether a written type names no value with `typeof`, and TypeDoc can convert it as written.
function literal(node: TypeNode): boolean {
	const query = (at: TypeNode & { isTypeOf?: boolean; typeArguments?: unknown[] }): true | undefined => {
		if (at.kind === ts.SyntaxKind.TypeQuery) return true;
		if (at.kind === ts.SyntaxKind.ImportType && (at.isTypeOf || at.typeArguments)) return true;
		return ts.forEachChild(at, query);
	};
	return !query(node);
}

// Where a declaration is exported: an entrypoint's import specifier, and its dotted path there.
interface Location {
	specifier: string;
	path: string;
}

// A declaration's identity, the same for TypeDoc's reference to it and the symbol it came from.
function key(id: ReflectionSymbolId): string {
	return `${id.packageName}\0${id.packagePath}\0${id.qualifiedName}`;
}

// Names every reference by where it is exported, as a placeholder `report` resolves once it knows
// which names its module imports. The standard library is global, and a type parameter is local.
function locate(project: ProjectReflection, modules: Map<Reflection, string>, exported: Map<string, Location>) {
	const find = (type: ReferenceType): Location | undefined => {
		if (type.refersToTypeParameter) return undefined;
		const target = type.reflection;
		if (target) {
			let module: Reflection | undefined = target;
			while (module && !modules.has(module)) module = module.parent;
			const specifier = module && modules.get(module);
			if (!module || !specifier) return undefined;
			return { specifier, path: target.getFullName().slice(module.getFullName().length + 1) };
		}
		const id = type.symbolId;
		if (!id) return undefined;
		if (id.packageName === "typescript") return { specifier: "", path: type.name };
		// A declaration its package does not export is named by its file, which is still exact.
		return exported.get(key(id)) ?? { specifier: `${id.packageName}/${id.packagePath}`, path: id.qualifiedName };
	};
	// Types hang off reflections in many places, so walk everything the project holds.
	const seen = new Set<object>();
	const visit = (value: unknown) => {
		if (!value || typeof value !== "object" || seen.has(value) || value instanceof ProjectReflection) return;
		seen.add(value);
		if (value instanceof ReferenceType) {
			const at = find(value);
			if (at) value.name = `\0${at.specifier}\0${at.path}\0`;
		}
		for (const [field, child] of Object.entries(value)) if (field !== "parent") visit(child);
	};
	for (const reflection of Object.values(project.reflections)) visit(reflection);
}

// A reference `locate` named: its import specifier, empty for the standard library, and its path.
const placeholder = /\0([^\0]*)\0([^\0]*)\0/g;

// The declarations that give a name each meaning TypeScript looks one up by.
const TYPE = ReflectionKind.Class | ReflectionKind.Interface | ReflectionKind.TypeAlias | ReflectionKind.Enum;
const NAMESPACE = ReflectionKind.Namespace | ReflectionKind.Module | ReflectionKind.Enum;
const VALUE = NAMESPACE | ReflectionKind.Class | ReflectionKind.Variable | ReflectionKind.Function;

// Every package the manifest depends on.
function dependencies(manifest: Manifest): string[] {
	const fields = ["dependencies", "peerDependencies", "optionalDependencies"] as const;
	return [...new Set(fields.flatMap((field) => Object.keys(manifest[field] ?? {})))];
}

// The import specifier of each entrypoint of package `name`, as installed for `dir`.
function entrypoints(dir: string, name: string): string[] {
	const installed = installation(dir, name);
	if (!installed) return [];
	const manifest: Manifest = JSON.parse(readFileSync(join(installed, "package.json"), "utf8"));
	if (!manifest.exports) return [name];
	return Object.keys(manifest.exports)
		.filter((subpath) => subpath.startsWith(".") && !subpath.includes("*"))
		.map((subpath) => (subpath === "." ? name : `${name}/${subpath.slice(2)}`));
}

// Every reference to a declaration of this package that the report cannot name, since no
// entrypoint exports it: a consumer could not name it either. Another package's declarations are
// its own report's concern.
function unexported(app: Application, project: ProjectReflection, name: string): string[] {
	const found = new Set<string>();
	const visit = (value: unknown, owner: string) => {
		if (Array.isArray(value)) {
			for (const item of value) visit(item, owner);
		} else if (value && typeof value === "object") {
			const node = value as Record<string, unknown>;
			const target = node.target as { packageName?: string; qualifiedName?: string } | number | undefined;
			const declared = node.type === "reference" && !node.refersToTypeParameter && typeof target === "object";
			if (declared && target.packageName === name) {
				found.add(`${target.qualifiedName}, used by ${owner}, is not exported`);
			}
			const named = node.variant === "declaration" && node.kind !== ReflectionKind.Module;
			const next = named ? (owner ? `${owner}.${node.name}` : String(node.name)) : owner;
			for (const child of Object.values(node)) visit(child, next);
		}
	};
	visit(app.serializer.projectToObject(project, "/"), "");
	return [...found].sort();
}

// Install each declared dependency where the copy resolves it, linked to the workspace's own
// install. Anything undeclared stays unresolvable, as it is for a consumer.
function linkDependencies(dir: string, root: string, manifest: Manifest) {
	for (const name of dependencies(manifest)) {
		const installed = installation(dir, name);
		if (!installed) throw new Error(`${name} is declared but not installed`);
		const link = join(root, "node_modules", name);
		mkdirSync(dirname(link), { recursive: true });
		symlinkSync(realpathSync(installed), link, "dir");
	}
}

// Where Node resolves the package `name` from `dir`.
function installation(dir: string, name: string): string | undefined {
	for (let at = dir; ; at = dirname(at)) {
		const candidate = join(at, "node_modules", name);
		if (existsSync(candidate)) return realpathSync(candidate);
		if (dirname(at) === at) return undefined;
	}
}

// Renders a module as TypeScript declarations, one member per line and sorted by name, so a diff
// shows exactly what changed. Bodies of another package's declarations are left to its own report.
class Renderer {
	#project: ProjectReflection;
	// The package being reported, its entrypoint modules by import specifier, and its `const`s.
	#name: string;
	#modules: Map<Reflection, string>;
	#constants: Set<Reflection>;
	// Where the dependencies export their declarations.
	#exported: Map<string, Location>;
	// Where the package is installed, which resolves its dependencies.
	#dir: string;

	constructor(scope: {
		project: ProjectReflection;
		name: string;
		modules: Map<Reflection, string>;
		constants: Set<Reflection>;
		exported: Map<string, Location>;
		dir: string;
	}) {
		this.#project = scope.project;
		this.#name = scope.name;
		this.#modules = scope.modules;
		this.#constants = scope.constants;
		this.#exported = scope.exported;
		this.#dir = scope.dir;
	}

	report(module: DeclarationReflection): string {
		const specifier = this.#modules.get(module);
		const body = this.children(module, "").join("\n\n");
		// A reference elsewhere is imported by the first name of its path, unless that name is
		// declared here or comes from two places, when it is spelled with `import()` instead.
		const sources = new Map<string, Set<string>>();
		for (const child of module.children ?? []) sources.set(child.name, new Set([specifier ?? ""]));
		for (const [, from = "", path = ""] of body.matchAll(placeholder)) {
			const [head = ""] = path.split(".");
			sources.set(head, (sources.get(head) ?? new Set()).add(from));
		}
		const imported = (from: string, head: string) =>
			from !== specifier && from !== "" && head !== "default" && sources.get(head)?.size === 1;
		const imports = new Set<string>();
		const text = body.replace(placeholder, (_, from: string, path: string) => {
			const [head = ""] = path.split(".");
			// A default export has no name to write, even in its own module.
			if (from === "" || (from === specifier && head !== "default")) return path;
			if (!imported(from, head)) return `import("${from}").${path}`;
			imports.add(`import { ${head} } from "${from}";`);
			return path;
		});
		const header = [...imports].sort().join("\n");
		return `# \`${specifier}\`\n\n\`\`\`ts\n${header ? `${header}\n\n` : ""}${text}\n\`\`\`\n`;
	}

	// Each child's declaration, once: a merged class and namespace from another package are two
	// reflections but one re-export.
	private children(container: DeclarationReflection, indent: string): string[] {
		const found = sorted(container.children ?? []).map((child) => this.declaration(child, indent).join("\n"));
		return found.filter((text, i) => text !== found[i - 1]);
	}

	private declaration(item: DeclarationReflection, indent: string): string[] {
		const found = this.named(item, indent);
		if (item.name !== "default") return found;
		// A default export is spelled as the declarations TypeScript emits for it.
		if (item.kind === ReflectionKind.Variable) {
			return [
				`${indent}declare const _default: ${print(item.type, indent)};`,
				`${indent}export default _default;`,
			];
		}
		const [head = "", ...rest] = found;
		return [
			head.replace(/^(\s*)export (abstract class|class|function|interface) default/, "$1export default $2"),
			...rest,
		];
	}

	private named(item: DeclarationReflection, indent: string): string[] {
		const parameters = typeParameters(item.typeParameters, indent);
		const inner = `${indent}\t`;
		const block = (head: string, lines: string[], end = "}") =>
			lines.length ? [`${indent}${head} {`, ...lines, `${indent}${end}`] : [`${indent}${head} {${end}`];
		if (item instanceof ReferenceReflection) return [`${indent}${this.reexport(item)}`];
		const foreign = this.foreign(item);
		if (foreign) return [`${indent}${foreign}`];

		switch (item.kind) {
			case ReflectionKind.Namespace:
				return block(`export namespace ${item.name}`, this.unshadow(item, this.children(item, inner)));
			case ReflectionKind.Enum:
				return block(
					`export ${item.flags.isConst ? "const " : ""}enum ${item.name}`,
					sorted(item.children ?? []).map(
						(member) => `${inner}${member.name} = ${print(member.type, inner)},`,
					),
				);
			case ReflectionKind.Class: {
				const heritage =
					clause("extends", item.extendedTypes, indent) + clause("implements", item.implementedTypes, indent);
				const abstract = item.flags.isAbstract ? "abstract " : "";
				return block(`export ${abstract}class ${item.name}${parameters}${heritage}`, lines(item, inner));
			}
			case ReflectionKind.Interface:
				return block(
					`export interface ${item.name}${parameters}${clause("extends", item.extendedTypes, indent)}`,
					lines(item, inner),
				);
			case ReflectionKind.TypeAlias: {
				const literal = item.children ? item : objectLiteral(item.type);
				if (literal) return block(`export type ${item.name}${parameters} =`, lines(literal, inner), "};");
				return [`${indent}export type ${item.name}${parameters} = ${print(item.type, indent)};`];
			}
			case ReflectionKind.Variable: {
				const keyword = this.#constants.has(item) ? "const" : "let";
				return [`${indent}export ${keyword} ${item.name}: ${print(item.type, indent)};`];
			}
			case ReflectionKind.Function:
				return (item.signatures ?? []).map(
					(signature) => `${indent}export function ${item.name}${call(signature, ": ", indent)};`,
				);
			default:
				throw new Error(
					`${item.getFullName()} is a ${ReflectionKind[item.kind]}, which the report cannot render`,
				);
		}
	}

	// A name a namespace declares hides the same name outside it, from a reference that looks the
	// name up by a meaning it has: a type, a namespace before a dot, or a value after `typeof`. Such
	// a reference is spelled from its module instead. Re-exporting the outer declaration hides nothing.
	private unshadow(namespace: DeclarationReflection, body: string[]): string[] {
		const hiding = new Map<string, number>();
		const outer = new Set<string>();
		for (const child of namespace.children ?? []) {
			const target = child instanceof ReferenceReflection ? child.tryGetTargetReflectionDeep() : child;
			if (!target) continue;
			const module = this.module(target);
			if (child !== target && target.parent === module && target.name === child.name) {
				outer.add(`${this.#modules.get(module)}\0${child.name}`);
			} else {
				hiding.set(child.name, (hiding.get(child.name) ?? 0) | target.kind);
			}
		}
		return body.map((text) =>
			text.replace(/(typeof )?\0([^\0]*)\0([^\0]*)\0/g, (found, query = "", from: string, path: string) => {
				const [head = "", ...rest] = path.split(".");
				const by = query ? VALUE : rest.length ? NAMESPACE : TYPE;
				if (!((hiding.get(head) ?? 0) & by) || outer.has(`${from}\0${head}`)) return found;
				return `${query}${from ? `import("${from}")` : "globalThis"}.${path}`;
			}),
		);
	}

	// A re-export of something this package declares under another name or in another entrypoint.
	private reexport(item: ReferenceReflection): string {
		const target = item.tryGetTargetReflectionDeep();
		if (!target) throw new Error(`${item.getFullName()} re-exports nothing`);
		const foreign = this.foreign(target, item.name);
		if (foreign) return foreign;
		const specifier = this.#modules.get(target);
		if (specifier) return `export * as ${item.name} from "${specifier}";`;
		const module = this.module(target);
		const name = target.getFullName().slice(module.getFullName().length + 1);
		const from = module === this.module(item) ? "" : ` from "${this.#modules.get(module)}"`;
		// A namespace member can only be re-exported as an alias of its qualified name.
		if (!from && name.includes(".")) return `export import ${item.name} = ${name};`;
		return `export { ${alias(name, item.name)} }${from};`;
	}

	// A declaration another package owns, as the re-export that brings it here.
	private foreign(item: Reflection, name = item.name): string | undefined {
		const id = this.#project.getSymbolIdFromReflection(item);
		if (!id || id.packageName === this.#name) return undefined;
		// A whole module has no name of its own.
		if (!id.qualifiedName) return `export * as ${name} from "${this.specifier(id.packageName, id.packagePath)}";`;
		const at = this.#exported.get(key(id));
		if (!at)
			throw new Error(
				`${item.getFullName()} re-exports ${id.qualifiedName}, which ${id.packageName} does not export`,
			);
		if (at.path.includes(".")) {
			throw new Error(
				`${item.getFullName()} re-exports ${at.path} from ${at.specifier}, which the report cannot spell`,
			);
		}
		return `export { ${alias(at.path, name)} } from "${at.specifier}";`;
	}

	// The entrypoint of package `name` whose file is `path`, or the package itself.
	private specifier(name: string, path: string): string {
		const dir = installation(this.#dir, name);
		const manifest: Manifest | undefined = dir && JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
		for (const [subpath, target] of Object.entries(manifest?.exports ?? {})) {
			const file = typeof target === "string" ? target : target.types;
			if (file?.replace(/^\.\//, "") === path) return subpath === "." ? name : `${name}/${subpath.slice(2)}`;
		}
		return name;
	}

	private module(item: Reflection): Reflection {
		let at = item;
		while (at.parent && !this.#modules.has(at)) at = at.parent;
		return at;
	}
}

// Sorted by name, then kind, so moving a declaration in its file changes nothing.
function sorted<T extends Reflection>(items: T[]): T[] {
	return [...items].sort((a, b) => (a.name === b.name ? a.kind - b.kind : a.name < b.name ? -1 : 1));
}

function alias(name: string, as: string): string {
	return name === as ? name : `${name} as ${as}`;
}

function clause(keyword: string, types: SomeType[] | undefined, indent: string): string {
	return types?.length ? ` ${keyword} ${types.map((type) => print(type, indent)).join(", ")}` : "";
}

// An object type spelled out member by member, rather than a function type or an empty `{}`.
function objectLiteral(type?: SomeType): DeclarationReflection | undefined {
	if (type?.type !== "reflection") return undefined;
	const { declaration } = type;
	return declaration.children || declaration.indexSignatures ? declaration : undefined;
}

function typeParameters(parameters: TypeParameterReflection[] | undefined, indent: string): string {
	if (!parameters?.length) return "";
	const each = parameters.map((parameter) => {
		const modifiers = `${parameter.flags.isConst ? "const " : ""}${parameter.varianceModifier ? `${parameter.varianceModifier} ` : ""}`;
		const constraint = parameter.type ? ` extends ${print(parameter.type, indent)}` : "";
		const fallback = parameter.default ? ` = ${print(parameter.default, indent)}` : "";
		return `${modifiers}${parameter.name}${constraint}${fallback}`;
	});
	return `<${each.join(", ")}>`;
}

function parameter(item: ParameterReflection, indent: string): string {
	// TypeDoc names a destructured parameter `__namedParameters`; its fields are in its type.
	const name = item.name === "__namedParameters" ? "{ ... }" : item.name;
	const optional = item.flags.isOptional || item.defaultValue !== undefined ? "?" : "";
	return `${item.flags.isRest ? "..." : ""}${name}${optional}: ${print(item.type, indent)}`;
}

// A signature's parameters and return type, which `arrow` separates: `: ` for a declaration and
// ` => ` for a function type. A setter has no return type.
function call(signature: SignatureReflection, arrow: string | undefined, indent: string, generic = true): string {
	// A class's type parameters are its own, though TypeDoc repeats them on its constructor.
	const types = generic ? typeParameters(signature.typeParameters, indent) : "";
	const parameters = (signature.parameters ?? []).map((item) => parameter(item, indent)).join(", ");
	const returns = arrow === undefined ? "" : `${arrow}${print(signature.type, indent)}`;
	return `${types}(${parameters})${returns}`;
}

function lines(container: DeclarationReflection, indent: string): string[] {
	return members(container, indent).map((member) => `${indent}${member};`);
}

// A class, interface, or object type's members, each printed at `indent`: signatures and the
// constructor first, then by name.
function members(container: DeclarationReflection, indent: string): string[] {
	const found = [
		...(container.signatures ?? []).map((signature) => call(signature, ": ", indent)),
		...(container.indexSignatures ?? []).map((signature) => {
			const [key] = signature.parameters ?? [];
			return `${modifiers(signature)}[${key?.name}: ${print(key?.type, indent)}]: ${print(signature.type, indent)}`;
		}),
	];
	const rest: string[] = [];
	for (const member of sorted(container.children ?? [])) {
		// Inherited members belong to the declaration they come from.
		if (member.inheritedFrom) continue;
		const prefix = modifiers(member);
		const optional = member.flags.isOptional ? "?" : "";
		switch (member.kind) {
			case ReflectionKind.Constructor:
				for (const signature of member.signatures ?? []) {
					const isClass = container.kind === ReflectionKind.Class;
					const text = isClass
						? `constructor${call(signature, undefined, indent, false)}`
						: `new ${call(signature, ": ", indent)}`;
					found.push(`${prefix}${text}`);
				}
				break;
			case ReflectionKind.Property:
				rest.push(`${prefix}${member.name}${optional}: ${print(member.type, indent)}`);
				break;
			case ReflectionKind.Method:
				for (const signature of member.signatures ?? []) {
					rest.push(`${prefix}${member.name}${optional}${call(signature, ": ", indent)}`);
				}
				break;
			case ReflectionKind.Accessor:
				if (member.getSignature)
					rest.push(`${prefix}get ${member.name}(): ${print(member.getSignature.type, indent)}`);
				if (member.setSignature)
					rest.push(`${prefix}set ${member.name}${call(member.setSignature, undefined, indent)}`);
				break;
			default:
				throw new Error(
					`${member.getFullName()} is a ${ReflectionKind[member.kind]}, which the report cannot render`,
				);
		}
	}
	return [...found, ...rest];
}

function modifiers(item: Reflection): string {
	const { flags } = item;
	return `${flags.isProtected ? "protected " : ""}${flags.isStatic ? "static " : ""}${flags.isAbstract ? "abstract " : ""}${flags.isReadonly ? "readonly " : ""}`;
}

// Where a type is printed, for the parentheses TypeScript's precedence needs there.
type Position = "top" | "union" | "intersection" | "operand" | "check";

// A type as TypeScript spells it. TypeDoc's own `toString` leaves a function type unparenthesized
// in an intersection and drops `readonly` from object members, which would misstate the API.
function print(type: SomeType | undefined, indent: string, at: Position = "top"): string {
	if (!type) return "any";
	const wrap = (text: string, inside: Position[]) => (inside.includes(at) ? `(${text})` : text);
	switch (type.type) {
		case "array":
			return `${print(type.elementType, indent, "operand")}[]`;
		case "conditional":
			return wrap(
				`${print(type.checkType, indent, "check")} extends ${print(type.extendsType, indent, "check")} ? ${print(type.trueType, indent)} : ${print(type.falseType, indent)}`,
				["union", "intersection", "operand", "check"],
			);
		case "indexedAccess":
			return `${print(type.objectType, indent, "operand")}[${print(type.indexType, indent)}]`;
		case "inferred":
			return wrap(
				`infer ${type.name}${type.constraint ? ` extends ${print(type.constraint, indent, "check")}` : ""}`,
				["operand"],
			);
		case "intersection":
			return wrap(type.types.map((member) => print(member, indent, "intersection")).join(" & "), ["operand"]);
		case "union":
			return wrap(type.types.map((member) => print(member, indent, "union")).join(" | "), [
				"intersection",
				"operand",
			]);
		case "intrinsic":
			return type.name;
		case "unknown":
			if (type.name === "...") throw new Error("TypeDoc cut a type short");
			return type.name;
		case "literal":
			return type.toString();
		case "mapped": {
			const readonly = type.readonlyModifier === "-" ? "-readonly " : type.readonlyModifier ? "readonly " : "";
			const optional = type.optionalModifier === "-" ? "-?" : type.optionalModifier ? "?" : "";
			const as = type.nameType ? ` as ${print(type.nameType, indent)}` : "";
			return `{ ${readonly}[${type.parameter} in ${print(type.parameterType, indent)}${as}]${optional}: ${print(type.templateType, indent)} }`;
		}
		case "namedTupleMember":
			return `${type.name}${type.isOptional ? "?" : ""}: ${print(type.element, indent)}`;
		case "optional":
			return `${print(type.elementType, indent, "operand")}?`;
		case "predicate":
			return `${type.asserts ? "asserts " : ""}${type.name}${type.targetType ? ` is ${print(type.targetType, indent)}` : ""}`;
		case "query":
			return `typeof ${print(type.queryType, indent)}`;
		case "reference": {
			const args = type.typeArguments?.length
				? `<${type.typeArguments.map((arg) => print(arg, indent)).join(", ")}>`
				: "";
			// `locate` named it by where it is exported, such as through an `@reexport` alias.
			return `${type.name}${args}`;
		}
		case "reflection": {
			const { declaration } = type;
			const [signature, ...others] = declaration.signatures ?? [];
			if (signature && others.length === 0 && !declaration.children && !declaration.indexSignatures) {
				const keyword = signature.kind === ReflectionKind.ConstructorSignature ? "new " : "";
				return wrap(`${keyword}${call(signature, " => ", indent)}`, [
					"union",
					"intersection",
					"operand",
					"check",
				]);
			}
			// One member per line, as a declaration's are, so a change to one is a one-line diff.
			const inner = `${indent}\t`;
			const entries = lines(declaration, inner);
			return entries.length ? `{\n${entries.join("\n")}\n${indent}}` : "{}";
		}
		case "rest":
			return `...${print(type.elementType, indent, "operand")}`;
		case "templateLiteral":
			return `\`${type.head}${type.tail.map(([part, text]) => `\${${print(part, indent)}}${text}`).join("")}\``;
		case "tuple":
			return `[${type.elements.map((element) => print(element, indent)).join(", ")}]`;
		case "typeOperator":
			return wrap(`${type.operator} ${print(type.target, indent, "operand")}`, ["operand"]);
	}
}

// Emit the declarations of a source package in `dir` into `out`, with a manifest whose exports
// name them, as the build's `dist/` would.
function declare(dir: string, out: string, manifest: Manifest) {
	const exports: Record<string, { types: string }> = {};
	let emit = false;
	for (const [subpath, target] of Object.entries(manifest.exports ?? {})) {
		const file = typeof target === "string" ? target : target.types;
		if (!file || file.endsWith(".css")) continue;
		if (file.endsWith(".d.ts")) {
			// Hand-written declarations, such as @moq/libav's, ship as they are.
			mkdirSync(dirname(join(out, file)), { recursive: true });
			cpSync(join(dir, file), join(out, file));
			exports[subpath] = { types: file };
		} else {
			exports[subpath] = { types: file.replace(/^\.\/src\//, "./").replace(/\.tsx?$/, ".d.ts") };
			emit = true;
		}
	}
	if (emit) {
		// Its exit status is ignored: a type error still emits, and the commit this renders was
		// checked when it was built.
		Bun.spawnSync(
			[
				"bunx",
				"tsc",
				"-p",
				"tsconfig.build.json",
				"--emitDeclarationOnly",
				"--declarationMap",
				"false",
				"--outDir",
				out,
			],
			{ cwd: dir, stdio: ["ignore", "inherit", "inherit"] },
		);
	}
	for (const { types } of Object.values(exports)) {
		if (!existsSync(join(out, types))) throw new Error(`${manifest.name} emitted no ${types}`);
	}
	writeFileSync(join(out, "package.json"), JSON.stringify({ ...manifest, exports }));
}

if (import.meta.main) {
	const [command, ...args] = process.argv.slice(2);
	const [checkout, out] = args;
	if (command === "check" && args.length > 0) {
		const stale: string[] = [];
		for (const dir of args) stale.push(...(await checkReports(dir)));
		if (stale.length > 0) {
			console.error("The public API no longer matches its committed report; run `just fix` and review the diff:");
			for (const file of stale) console.error(`  ${file}`);
			process.exit(1);
		}
	} else if (command === "baseline" && checkout && out && args.length === 2) {
		await baseline(checkout, out);
	} else {
		console.error("usage: bun js/common/api.ts check <dir>... | baseline <checkout> <out>");
		process.exit(2);
	}
}

// Render the report of every published package in `checkout` into `out`.
async function baseline(checkout: string, out: string) {
	for (const entry of readdirSync(join(checkout, "js"), { withFileTypes: true })) {
		const dir = resolve(checkout, "js", entry.name);
		if (!entry.isDirectory() || !existsSync(join(dir, "package.json"))) continue;
		const manifest: Manifest = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
		if (!manifest.scripts?.release) continue;
		const work = mkdtempSync(join(tmpdir(), "moq-api-baseline-"));
		try {
			declare(dir, join(work, "dist"), manifest);
			const found = await reports(dir, join(work, "dist"), false);
			mkdirSync(join(out, manifest.name), { recursive: true });
			for (const [file, report] of found) writeFileSync(join(out, manifest.name, file), report);
		} finally {
			rmSync(work, { recursive: true, force: true });
		}
	}
}

// The API Extractor setup every `@moq/*` package shares: one report per `exports` entrypoint,
// committed beside the package and diffed by the release gate, with every message an error.
//
// The built `dist/` is analyzed in a copy outside the repository whose `node_modules` holds only
// the dependencies its manifest declares, as a consumer's install would. A workspace dependency
// resolves to its source, so a package's report never depends on whether its dependencies are built.

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
import { CompilerState, Extractor, ExtractorConfig, ExtractorLogLevel } from "@microsoft/api-extractor";

/**
 * Compare the built package in `dir` with its committed reports in `api/`, exiting on a
 * difference, or rewrite them when `MOQ_API_REPORT=update`, as `just fix` sets.
 */
export function checkReports(dir: string) {
	const folder = join(dir, "api");
	const current = reports(resolve(dir));
	const committed = existsSync(folder) ? readdirSync(folder).filter((file) => file.endsWith(".api.md")) : [];
	const stale = [...new Set([...current.keys(), ...committed])].filter((file) => {
		const path = join(folder, file);
		return current.get(file) !== (existsSync(path) ? readFileSync(path, "utf8") : undefined);
	});

	if (process.env.MOQ_API_REPORT === "update") {
		mkdirSync(folder, { recursive: true });
		for (const file of stale) {
			const report = current.get(file);
			if (report === undefined) rmSync(join(folder, file));
			else writeFileSync(join(folder, file), report);
		}
	} else if (stale.length > 0) {
		console.error("The public API no longer matches its committed report; run `just fix` and review the diff:");
		for (const file of stale) console.error(`  api/${file}`);
		process.exit(1);
	}
}

/** Every entrypoint's API report for the package built in `dir`, by file name such as `index.api.md`. */
function reports(dir: string): Map<string, string> {
	const work = mkdtempSync(join(tmpdir(), "moq-api-"));
	try {
		const root = join(work, "package");
		cpSync(join(dir, "dist"), root, { recursive: true });
		writeFileSync(
			join(root, "tsdoc.json"),
			JSON.stringify({
				$schema: "https://developer.microsoft.com/json-schemas/tsdoc/v0/tsdoc.schema.json",
				extends: [join(import.meta.dir, "tsdoc.json")],
			}),
		);
		prepareDeclarations(root);

		const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
		linkDependencies(dir, root, manifest);

		const folder = join(work, "reports");
		mkdirSync(folder);

		const entries = Object.entries<{ types?: string }>(manifest.exports ?? {}).flatMap(([subpath, target]) => {
			if (!target.types) return [];
			const entry = subpath === "." ? "index" : subpath.replace(/^\.\//, "").replaceAll("/", "-");
			const entrypoint = join(root, target.types);
			return [{ entry, entrypoint, config: configure(root, entrypoint, folder, entry) }];
		});
		const [first] = entries;
		if (!first) return new Map();

		// One program serves every entrypoint, so the dependencies are type-checked once.
		const compilerState = CompilerState.create(first.config, {
			additionalEntryPoints: entries.map(({ entrypoint }) => entrypoint),
		});
		const named = nameable(
			compilerState.program as Program,
			entries.map(({ entrypoint }) => entrypoint),
		);

		const found = new Map<string, string>();
		for (const { entry, config } of entries) {
			found.set(`${entry}.api.md`, extract(root, config, compilerState, named, folder, entry));
		}
		return found;
	} finally {
		rmSync(work, { recursive: true, force: true });
	}
}

// Install each declared dependency where the copy resolves it, linked to the workspace's own
// install. Anything undeclared stays unresolvable, as it is for a consumer.
function linkDependencies(dir: string, root: string, manifest: Record<string, Record<string, string> | undefined>) {
	const names = new Set(
		["dependencies", "peerDependencies", "optionalDependencies"].flatMap((field) =>
			Object.keys(manifest[field] ?? {}),
		),
	);
	for (const name of names) {
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
		if (existsSync(candidate)) return candidate;
		if (dirname(at) === at) return undefined;
	}
}

// The parts of the TypeScript compiler API used here. API Extractor types its program `unknown`
// rather than expose the TypeScript it bundles.
interface Program {
	getTypeChecker(): Checker;
	getSourceFile(path: string): SourceFile | undefined;
}
interface Checker {
	getSymbolAtLocation(node: Node): Symbol | undefined;
	getAliasedSymbol(symbol: Symbol): Symbol;
	getExportsOfModule(symbol: Symbol): Symbol[];
}
interface Node {
	kind: number;
	end: number;
	parent: Node;
	getChildren(): Node[];
	getStart(): number;
	getText(): string;
	getSourceFile(): SourceFile;
}
interface SourceFile extends Node {
	fileName: string;
	getPositionOfLineAndCharacter(line: number, character: number): number;
}
interface Symbol {
	flags: number;
	name: string;
	declarations?: Node[];
}

// The TypeScript that API Extractor bundles and built the program with.
const ts: {
	SymbolFlags: { Alias: number; Namespace: number };
	SyntaxKind: Record<
		| "Identifier"
		| "SourceFile"
		| "TypeAliasDeclaration"
		| "PropertySignature"
		| "PropertyDeclaration"
		| "VariableDeclaration"
		| "TypeReference"
		| "TypeQuery",
		number
	>;
} = createRequire(import.meta.resolve("@microsoft/api-extractor"))("typescript");

// What a name resolves to, through any imports.
function target(checker: Checker, symbol: Symbol): Symbol {
	return symbol.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(symbol) : symbol;
}

// The declaration a declaration only renames: a type alias of a type (`type A = B`), or a property
// or variable holding a value (`static parse: typeof parse`), which is then reachable by the new name.
function renamed(checker: Checker, node: Node): Symbol | undefined {
	const { type } = node as Node & { type?: Node };
	if (!type) return undefined;
	const { typeArguments, typeName, exprName } = type as Node & {
		typeArguments?: unknown;
		typeName?: Node;
		exprName?: Node;
	};
	let name: Node | undefined;
	if (node.kind === ts.SyntaxKind.TypeAliasDeclaration && type.kind === ts.SyntaxKind.TypeReference) {
		name = typeName;
	} else if (
		[
			ts.SyntaxKind.PropertySignature,
			ts.SyntaxKind.PropertyDeclaration,
			ts.SyntaxKind.VariableDeclaration,
		].includes(node.kind) &&
		type.kind === ts.SyntaxKind.TypeQuery
	) {
		name = exprName;
	}
	const symbol = name && !typeArguments ? checker.getSymbolAtLocation(name) : undefined;
	return symbol && target(checker, symbol);
}

/**
 * Every declaration a consumer can name from one of `entrypoints`: an export, a namespace member,
 * or a declaration one of those renames, such as a class's `static parse: typeof parse`.
 */
function nameable(program: Program, entrypoints: string[]): Set<Symbol> {
	const checker = program.getTypeChecker();
	const found = new Set<Symbol>();
	const add = (symbol: Symbol) => {
		if (found.has(symbol)) return;
		found.add(symbol);
		if (symbol.flags & ts.SymbolFlags.Namespace) {
			for (const exported of checker.getExportsOfModule(symbol)) add(target(checker, exported));
		}
		const visit = (node: Node) => {
			const symbol = renamed(checker, node);
			if (symbol) add(symbol);
			node.getChildren().forEach(visit);
		};
		for (const declaration of symbol.declarations ?? []) {
			// A module's declaration is its file, whose exports are added above.
			if (declaration.kind !== ts.SyntaxKind.SourceFile) visit(declaration);
		}
	};

	for (const entrypoint of entrypoints) {
		const file = program.getSourceFile(entrypoint);
		const module = file && checker.getSymbolAtLocation(file);
		if (!module) throw new Error(`${entrypoint} is not a module`);
		add(module);
	}
	return found;
}

/**
 * Whether everything in this package named `name` that the declaration starting at `line` and
 * `column` of `path` references is in `named`. API Extractor locates a forgotten export at the declaration that
 * references it, and names it as emitted, with a suffix such as `_2` when the name is taken, so
 * every candidate must be nameable.
 */
function nameableReference(
	program: Program,
	root: string,
	named: Set<Symbol>,
	path: string,
	line: number,
	column: number,
	name: string,
): boolean {
	const file = program.getSourceFile(path);
	if (!file) return false;
	const start = file.getPositionOfLineAndCharacter(line - 1, column - 1);
	const checker = program.getTypeChecker();
	const base = name.replace(/_\d+$/, "");

	// The outermost node starting there is the declaration.
	let declaration: Node | undefined;
	for (let node: Node = file; !declaration; ) {
		const child: Node | undefined = node
			.getChildren()
			.find((child) => child.getStart() <= start && start < child.end);
		if (!child) return false;
		if (child.getStart() === start) declaration = child;
		node = child;
	}

	// A declaration is named for itself, or an imported module for its local name, and an import
	// may rename either.
	const candidates = new Set<Symbol>();
	const visit = (node: Node) => {
		// A member's own name is a declaration, not a reference.
		if (node.kind === ts.SyntaxKind.Identifier && (node.parent as Node & { name?: Node }).name !== node) {
			const symbol = checker.getSymbolAtLocation(node);
			const resolved = symbol && target(checker, symbol);
			// API Extractor never asks to export another package's declaration.
			const local = resolved?.declarations?.some(
				(d) => !relative(root, d.getSourceFile().fileName).startsWith(".."),
			);
			if (resolved && local && (resolved.name === base || node.getText() === base)) candidates.add(resolved);
		}
		node.getChildren().forEach(visit);
	};
	visit(declaration);
	return candidates.size > 0 && [...candidates].every((symbol) => named.has(symbol));
}

// Keep JSR's module tag and expose its summary to API Extractor. A namespace re-export becomes
// the equivalent import-then-export: API Extractor cannot follow one from another package, and
// makes each one within the package a new namespace, distinct from the one its own files import.
function prepareDeclarations(root: string) {
	for (const rel of new Bun.Glob("**/*.d.ts").scanSync(root)) {
		const file = join(root, rel);
		const body = readFileSync(file, "utf8");
		const next = body
			.replace(/^(\s*\* @module)\s*$/gm, "$1\n * @packageDocumentation")
			.replace(
				/^export \* as ([A-Za-z_$][\w$]*) from "([^"]+)";$/gm,
				'import * as $1 from "$2";\nexport { $1 };',
			);
		if (next !== body) writeFileSync(file, next);
	}
}

function configure(root: string, entrypoint: string, folder: string, entry: string): ExtractorConfig {
	return ExtractorConfig.prepare({
		configObject: {
			projectFolder: root,
			newlineKind: "lf",
			mainEntryPointFilePath: entrypoint,
			compiler: {
				overrideTsconfig: {
					compilerOptions: {
						target: "esnext",
						module: "esnext",
						moduleResolution: "bundler",
						lib: ["esnext", "dom"],
						strict: true,
						types: [],
					},
					files: [entrypoint],
				},
			},
			apiReport: {
				enabled: true,
				reportFolder: folder,
				reportTempFolder: join(root, ".api-extractor"),
				reportFileName: entry,
				// A type the API references without exporting is still part of it, so a change to
				// its declaration must change the report.
				includeForgottenExports: true,
			},
			docModel: { enabled: false },
			dtsRollup: { enabled: false },
			tsdocMetadata: { enabled: false },
			messages: {
				compilerMessageReporting: { default: { logLevel: ExtractorLogLevel.Error } },
				extractorMessageReporting: {
					default: { logLevel: ExtractorLogLevel.Error },
					// `@internal` and `stripInternal` are the only visibility tier, so every
					// declaration left is public without a tag.
					"ae-missing-release-tag": { logLevel: ExtractorLogLevel.None },
					// JSR renders the docs and resolves a link in the comment's own scope, such as a
					// sibling member; API Extractor resolves one only from the entrypoint's exports.
					"ae-unresolved-link": { logLevel: ExtractorLogLevel.None },
				},
				tsdocMessageReporting: { default: { logLevel: ExtractorLogLevel.Error } },
			},
		},
		configObjectFullPath: join(root, "api-extractor.json"),
		packageJsonFullPath: join(root, "package.json"),
	});
}

function extract(
	root: string,
	config: ExtractorConfig,
	compilerState: CompilerState,
	named: Set<Symbol>,
	folder: string,
	entry: string,
): string {
	// localBuild writes the report into the scratch folder instead of failing because it is new,
	// and the console chatter about that is suppressed; errors still fail the result.
	const result = Extractor.invoke(config, {
		localBuild: true,
		compilerState,
		messageCallback: (message) => {
			const path = message.sourceFilePath;
			// A dependency's own declarations are checked by its own build; compiled here only for
			// their types, they answer to this package's compiler options, not their own.
			if (path && relative(root, path).startsWith("..")) message.logLevel = ExtractorLogLevel.None;
			// API Extractor wants each declaration a signature uses exported from this entrypoint,
			// but a consumer only needs to name it: from another entrypoint, such as a subpath's
			// from the package root, or through a rename, such as `Connection.Props`.
			const name = message.text.match(/^The symbol "(.+)" needs to be exported/)?.[1];
			const { sourceFileLine: line, sourceFileColumn: column } = message;
			if (
				message.messageId === "ae-forgotten-export" &&
				path &&
				name &&
				line &&
				column &&
				nameableReference(compilerState.program as Program, root, named, path, line, column, name)
			) {
				message.logLevel = ExtractorLogLevel.None;
			}
			if (message.logLevel !== ExtractorLogLevel.Error) message.handled = true;
		},
	});
	if (!result.succeeded) throw new Error(`API Extractor failed on ${entry}: ${result.errorCount} errors`);

	// Keep only the API: whether an item has a doc comment is not part of it, and every exported
	// item is @public, so neither marker churns the report or reads as a break.
	return readFileSync(join(folder, `${entry}.api.md`), "utf8")
		.replaceAll(" (undocumented)", "")
		.replace(/^ *\/\/( @public| \(No @packageDocumentation comment for this package\))?\n/gm, "");
}

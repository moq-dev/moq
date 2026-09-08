// Verifies that every workspace package declares the `@moq/*` packages it imports.
//
// A bundler resolves an undeclared import through hoisting, so the tree builds and
// the tests pass while a published consumer with a stricter resolver gets `undefined`.

import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "@babel/parser";

/** A package.json, narrowed to the fields this script reads. */
type Manifest = {
	name?: string;
	dependencies?: Record<string, string>;
	peerDependencies?: Record<string, string>;
	optionalDependencies?: Record<string, string>;
	devDependencies?: Record<string, string>;
	workspaces?: string[];
};

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

const SOURCE = /\.(m|c)?(ts|js)x?$/;
const TEST = /\.test\.(m|c)?(ts|js)x?$/;
const SKIP = new Set(["node_modules", "dist", "out", "pkg", ".git", ".vite"]);

function manifest(dir: string): Manifest {
	return JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
}

function sources(dir: string): string[] {
	const found: string[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		if (SKIP.has(entry.name)) continue;
		const path = join(dir, entry.name);
		if (entry.isDirectory()) found.push(...sources(path));
		else if (SOURCE.test(entry.name)) found.push(path);
	}
	return found;
}

type AstNode = { type: string; [key: string]: unknown };

function node(value: unknown): value is AstNode {
	return typeof value === "object" && value !== null && "type" in value;
}

function literal(value: unknown): string | undefined {
	if (!node(value)) return undefined;
	if (value.type === "StringLiteral" && typeof value.value === "string") return value.value;
	if (value.type !== "TemplateLiteral") return undefined;

	const expressions = value.expressions;
	const quasis = value.quasis;
	if (!Array.isArray(expressions) || expressions.length > 0 || !Array.isArray(quasis) || quasis.length !== 1) {
		return undefined;
	}

	const element = quasis[0];
	if (!node(element) || typeof element.value !== "object" || element.value === null) return undefined;
	const cooked = "cooked" in element.value ? element.value.cooked : undefined;
	return typeof cooked === "string" ? cooked : undefined;
}

function visit(value: unknown, found: string[]) {
	if (Array.isArray(value)) {
		for (const child of value) visit(child, found);
		return;
	}
	if (!node(value)) return;

	if (
		value.type === "ImportDeclaration" ||
		value.type === "ExportNamedDeclaration" ||
		value.type === "ExportAllDeclaration"
	) {
		const source = literal(value.source);
		if (source) found.push(source);
	} else if (value.type === "ImportExpression") {
		const source = literal(value.source);
		if (source) found.push(source);
	} else if (value.type === "TSImportType") {
		const source = literal(value.argument);
		if (source) found.push(source);
	} else if (value.type === "TSExternalModuleReference") {
		const source = literal(value.expression);
		if (source) found.push(source);
	} else if (value.type === "CallExpression") {
		const callee = value.callee;
		const args = value.arguments;
		if (
			node(callee) &&
			(callee.type === "Import" || (callee.type === "Identifier" && callee.name === "require")) &&
			Array.isArray(args)
		) {
			const source = literal(args[0]);
			if (source) found.push(source);
		}
	}

	for (const child of Object.values(value)) visit(child, found);
}

/** The module specifiers referenced by JavaScript or TypeScript source. */
export function specifiers(source: string): string[] {
	const ast = parse(source, {
		createImportExpressions: true,
		plugins: ["typescript", "jsx"],
		sourceType: "unambiguous",
	});
	const found: string[] = [];
	visit(ast, found);
	return found;
}

/** The package a specifier resolves to, or undefined when it is not a `@moq/*` import. */
function packageName(specifier: string): string | undefined {
	if (!specifier.startsWith("@moq/")) return undefined;
	const [scope, name] = specifier.split("/");
	return name ? `${scope}/${name}` : undefined;
}

function main() {
	const problems = new Set<string>();

	for (const workspace of manifest(root).workspaces ?? []) {
		const dir = join(root, workspace);
		const pkg = manifest(dir);

		const runtime = new Set([...Object.keys(pkg.dependencies ?? {}), ...Object.keys(pkg.peerDependencies ?? {})]);
		const dev = new Set([
			...runtime,
			...Object.keys(pkg.optionalDependencies ?? {}),
			...Object.keys(pkg.devDependencies ?? {}),
		]);

		for (const file of sources(dir)) {
			// Only files that ship need a runtime dependency; configs, examples, and
			// tests may lean on a devDependency instead.
			const shipped = relative(dir, file).startsWith(`src${sep}`) && !TEST.test(file);
			const declared = shipped ? runtime : dev;

			for (const specifier of specifiers(readFileSync(file, "utf8"))) {
				const name = packageName(specifier);
				if (!name || name === pkg.name || declared.has(name)) continue;

				const where = shipped ? "dependencies or peerDependencies" : "any dependency list";
				problems.add(`${relative(root, file)}: imports ${name}, missing from ${pkg.name} ${where}`);
			}
		}
	}

	if (problems.size > 0) {
		console.error(`❌ ${problems.size} undeclared @moq dependencies:`);
		for (const problem of [...problems].sort()) console.error(`   ${problem}`);
		process.exit(1);
	}

	console.log("✅ every imported @moq package is declared");
}

if (Bun.main === fileURLToPath(import.meta.url)) main();

// Verifies that every workspace package declares the `@moq/*` packages it imports.
//
// A bundler resolves an undeclared import through hoisting, so the tree builds and
// the tests pass while a published consumer with a stricter resolver gets `undefined`.

import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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

// Matches the specifier of a static import/export, a dynamic import, or a require.
// Anchoring on the keyword keeps strings like `Symbol.for("@moq/signals")` out.
const SPECIFIER = /(?:\bfrom|\bimport|\brequire)\s*\(?\s*["']([^"']+)["']/g;

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

/** The package a specifier resolves to, or undefined when it is not a `@moq/*` import. */
function packageName(specifier: string): string | undefined {
	if (!specifier.startsWith("@moq/")) return undefined;
	const [scope, name] = specifier.split("/");
	return name ? `${scope}/${name}` : undefined;
}

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
		const shipped = relative(dir, file).startsWith("src/") && !TEST.test(file);
		const declared = shipped ? runtime : dev;

		for (const [, specifier] of readFileSync(file, "utf8").matchAll(SPECIFIER)) {
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

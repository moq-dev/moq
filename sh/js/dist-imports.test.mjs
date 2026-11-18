import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { resolve } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

if (process.argv.length < 3) {
	throw new Error("usage: node sh/js/dist-imports.test.mjs WORKSPACE...");
}

const root = resolve(import.meta.dirname, "../..");

// A workspace dependency resolves to its source in this checkout, but a consumer installs its
// published dist, so load each `@moq/*` import from the built dist the way npm would.
const dists = new Map();
for (const directory of JSON.parse(readFileSync(resolve(root, "package.json"), "utf8")).workspaces) {
	const { name } = JSON.parse(readFileSync(resolve(root, directory, "package.json"), "utf8"));
	dists.set(name, resolve(root, directory, "dist"));
}

function manifest(dist) {
	const path = resolve(dist, "package.json");
	if (!existsSync(path)) throw new Error(`${dist} is not built`);
	return JSON.parse(readFileSync(path, "utf8"));
}

function entry(dist, subpath) {
	const target = manifest(dist).exports[subpath];
	if (target === undefined) throw new Error(`${dist} does not export ${subpath}`);
	return pathToFileURL(resolve(dist, typeof target === "string" ? target : target.default)).href;
}

registerHooks({
	resolve(specifier, context, next) {
		const match = /^(@moq\/[^/]+)(\/.*)?$/.exec(specifier);
		const dist = match && dists.get(match[1]);
		if (!dist) return next(specifier, context);
		return { url: entry(dist, `.${match[2] ?? ""}`), shortCircuit: true };
	},
});

for (const directory of process.argv.slice(2)) {
	const dist = resolve(root, directory, "dist");
	const pkg = manifest(dist);
	for (const subpath of Object.keys(pkg.exports)) {
		await test(`${pkg.name}${subpath.slice(1)} loads in Node.js`, async () => {
			await import(entry(dist, subpath));
		});
	}
}

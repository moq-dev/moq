import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

if (process.argv.length < 3) {
	throw new Error("usage: node sh/js/dist-imports.test.mjs DIST...");
}

for (const directory of process.argv.slice(2)) {
	const pkg = JSON.parse(readFileSync(resolve(directory, "package.json"), "utf8"));
	for (const [subpath, entry] of Object.entries(pkg.exports)) {
		const target = typeof entry === "string" ? entry : entry.default;
		await test(`${pkg.name}${subpath.slice(1)} loads in Node.js`, async () => {
			await import(pathToFileURL(resolve(directory, target)).href);
		});
	}
}

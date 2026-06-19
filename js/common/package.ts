// Script to build and package a workspace for distribution
// This creates a dist/ folder with the correct paths and dependencies for publishing
// Split from release.ts to allow building packages without publishing

import { copyFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { publint } from "publint";
import { formatMessage } from "publint/utils";

console.log("✍️  Rewriting package.json...");
const pkg = JSON.parse(readFileSync("package.json", "utf8"));

// Capture the source exports before the npm rewrite below mutates them, so the
// JSR config can map them to the built (.js) entrypoints.
const srcExports: Record<string, unknown> = structuredClone(pkg.exports ?? {});

// Publish to JSR alongside npm for every package that publishes at all (has a
// release script), unless it opts out with "jsr": false. The web-component
// packages opt out: JSR forbids the HTMLElementTagNameMap global augmentation
// every custom element needs. Captured before pkg.scripts/jsr are cleared below.
const publishJsr = Boolean(pkg.scripts?.release) && pkg.jsr !== false;

function rewritePath(p: string, ext: string): string {
	return p.replace(/^\.\/src/, ".").replace(/\.ts(x)?$/, `.${ext}`);
}

pkg.main &&= rewritePath(pkg.main, "js");
pkg.types &&= rewritePath(pkg.types, "d.ts");

if (pkg.exports) {
	for (const key in pkg.exports) {
		const val = pkg.exports[key];
		if (typeof val === "string") {
			if (val.endsWith(".css")) {
				// CSS exports are only needed for dev-time resolution;
				// consumers inline them at build time via @import.
				// We purposely do not copy them to the dist to help catch bugs.
				delete pkg.exports[key];
			} else {
				pkg.exports[key] = {
					types: rewritePath(val, "d.ts"),
					default: rewritePath(val, "js"),
				};
			}
		} else if (typeof val === "object") {
			for (const sub in val) {
				if (typeof val[sub] === "string") {
					val[sub] = rewritePath(val[sub], sub === "types" ? "d.ts" : "js");
				}
			}
		}
	}
}

if (pkg.sideEffects) {
	pkg.sideEffects = pkg.sideEffects.map((p: string) => rewritePath(p, "js"));
}

if (pkg.files) {
	pkg.files = pkg.files.map((p: string) => rewritePath(p, "js"));
}

if (pkg.bin) {
	if (typeof pkg.bin === "string") {
		pkg.bin = rewritePath(pkg.bin, "js");
	} else if (typeof pkg.bin === "object") {
		for (const key in pkg.bin) {
			pkg.bin[key] = rewritePath(pkg.bin[key], "js");
		}
	}
}

function rewriteWorkspaceDependency(dependencies?: Record<string, string>) {
	if (!dependencies) return;
	for (const [name, version] of Object.entries(dependencies)) {
		if (typeof version === "string" && version.startsWith("workspace:")) {
			// Read the actual version from the workspace package
			// Handle both scoped (@scope/name) and unscoped (name) packages
			const packageDir = name.includes("/") ? name.split("/")[1] : name;
			const workspacePkgPath = `../${packageDir}/package.json`;
			const workspacePkg = JSON.parse(readFileSync(workspacePkgPath, "utf8"));
			dependencies[name] = `^${workspacePkg.version}`;
			console.log(`🔗 Converted ${name}: ${version} → ^${workspacePkg.version}`);
		}
	}
}

// Convert workspace dependencies to published versions
rewriteWorkspaceDependency(pkg.dependencies);
rewriteWorkspaceDependency(pkg.devDependencies);
rewriteWorkspaceDependency(pkg.peerDependencies);

pkg.devDependencies = undefined;
pkg.scripts = undefined;
pkg.jsr = undefined; // JSR opt-out flag, not part of the npm package

// Write the rewritten package.json
writeFileSync("dist/package.json", JSON.stringify(pkg, null, 2));

// Copy static files
console.log("📄 Copying README.md...");
copyFileSync("README.md", join("dist", "README.md"));

// Lint the package to catch publishing issues
console.log("🔍 Running publint...");
const { messages, pkg: lintPkg } = await publint({
	pkgDir: resolve("dist"),
	level: "warning",
	pack: false,
});

if (messages.length > 0) {
	for (const message of messages) {
		console.error(formatMessage(message, lintPkg));
	}
	process.exit(1);
}

console.log("📦 Package built successfully in dist/");

// Optionally emit a jsr.json so the package can also publish to JSR (jsr.io).
// Generated from package.json so version/exports never drift. We publish the
// built dist (.js + .d.ts) rather than source: tsc emits explicit types into
// the .d.ts, which sidesteps JSR's "slow types" and ships real declarations,
// while JSR still builds the API docs from the .d.ts (JSDoc is preserved).
if (publishJsr) {
	writeJsrConfig();
}

function writeJsrConfig() {
	console.log("✍️  Generating jsr.json...");

	const exports: Record<string, string> = {};
	for (const [key, val] of Object.entries(srcExports)) {
		if (typeof val !== "string") continue;
		// CSS exports are dev-only and not published, same as the npm package.
		if (val.endsWith(".css")) continue;
		// rewritePath turns "./src/index.ts" into "./index.js".
		exports[key] = `./dist/${rewritePath(val, "js").slice(2)}`;
	}

	// Self-contained import map so we don't rely on JSR's package.json merge
	// behavior. Deps resolve via npm, so packages can publish to JSR in any
	// order. Flip @moq/* entries to "jsr:" once the whole graph is on JSR if you
	// want JSR-native cross-links between the docs.
	const imports: Record<string, string> = {};
	const deps = { ...(pkg.dependencies ?? {}), ...(pkg.peerDependencies ?? {}) };
	for (const [name, range] of Object.entries(deps) as [string, string][]) {
		if (name.startsWith("@types/")) continue; // type-only, never imported at runtime
		imports[name] = `npm:${name}@${range}`;
		// Trailing-slash subpath mapping (e.g. @moq/signals/dom). The leading slash
		// in "npm:/" is required: jsr.json's imports is a standalone import map, so
		// the value must parse as a base URL for relative resolution. The
		// "npm:name@range/" form (no slash) fails to URL-parse the appended subpath.
		imports[`${name}/`] = `npm:/${name}@${range}/`;
	}

	injectSelfTypes();
	rewriteDtsImports();

	// JSR validates the license field as a single recognized SPDX identifier, not
	// an expression: it rejects "(MIT OR Apache-2.0)". Take the first identifier
	// from a dual-license expression so the package keeps a valid license field.
	const license =
		typeof pkg.license === "string"
			? pkg.license
					.replace(/[()]/g, "")
					.split(/\s+(?:OR|AND)\s+/i)[0]
					.trim()
			: undefined;

	const jsr = {
		name: pkg.name,
		version: pkg.version,
		// JSR reads this for the package description and the "has a description"
		// score check; it isn't pulled from the npm package.json automatically.
		...(pkg.description ? { description: pkg.description } : {}),
		...(license ? { license } : {}),
		exports,
		...(Object.keys(imports).length ? { imports } : {}),
		// dist is gitignored, so un-ignore it with a "!" negation; JSR honors
		// .gitignore otherwise and would drop the whole build from the graph.
		publish: { include: ["dist", "README.md", "LICENSE*"], exclude: ["!dist"] },
	};

	writeFileSync("jsr.json", JSON.stringify(jsr, null, 2));
	console.log("📦 jsr.json written");
}

function rewriteDtsImports() {
	// JSR's doc generator resolves the .d.ts import graph and can't handle a
	// ".ts" extension (the dist ships .js/.d.ts, not .ts) or a bare "." specifier.
	// Normalize those in the declarations so doc generation resolves. Only the
	// .d.ts needs this; the .js keeps its own specifiers for npm consumers.
	const glob = new Bun.Glob("**/*.d.ts");
	for (const rel of glob.scanSync("dist")) {
		const file = join("dist", rel);
		const body = readFileSync(file, "utf8");
		const next = body
			.replace(/(from\s+"\.[^"]*?)\.ts"/g, '$1"') // "./x.ts" -> "./x"
			.replace(/(from\s+")\.(")/g, "$1./index$2"); // "." -> "./index"
		if (next !== body) writeFileSync(file, next);
	}
}

function injectSelfTypes() {
	// JSR ignores a sibling .d.ts unless the .js references it explicitly;
	// without this it infers types from the JS and reports "slow type" warnings.
	const glob = new Bun.Glob("**/*.js");
	for (const rel of glob.scanSync("dist")) {
		const js = join("dist", rel);
		const dts = js.replace(/\.js$/, ".d.ts");
		if (!existsSync(dts)) continue;
		const body = readFileSync(js, "utf8");
		if (body.includes("@ts-self-types")) continue;
		// Sibling .d.ts (same directory), so just its basename.
		const directive = `/* @ts-self-types="./${basename(dts)}" */\n`;
		// A shebang (bin entrypoints) must stay on line 1, so insert after it;
		// otherwise the directive goes first.
		if (body.startsWith("#!")) {
			const nl = body.indexOf("\n") + 1;
			writeFileSync(js, body.slice(0, nl) + directive + body.slice(nl));
		} else {
			writeFileSync(js, directive + body);
		}
	}
}

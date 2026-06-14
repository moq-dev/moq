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
// JSR config can choose between source (.ts) and built (.js) entrypoints.
const srcExports: Record<string, unknown> = structuredClone(pkg.exports ?? {});

// Per-package JSR mode lives in package.json ("jsr": "src" | "dist"); a --jsr
// flag overrides it. Captured here because we strip it from the npm package.json.
const jsrField: unknown = pkg.jsr;

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
pkg.jsr = undefined; // JSR-only field, not part of the npm package

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

// Optionally emit a jsr.json alongside package.json so the package can also
// publish to JSR (jsr.io). Generated from package.json so version/exports never
// drift. Mode comes from the package.json "jsr" field, overridable with --jsr:
//   "src"   publish the TypeScript source; JSR transpiles it and builds the
//           API reference from the source itself.
//   "dist"  publish the built dist, for packages that need Vite to inline
//           worklets/CSS/SVG which JSR cannot resolve from source.
const jsrFlag = process.argv.indexOf("--jsr");
const jsrMode = jsrFlag === -1 ? jsrField : process.argv[jsrFlag + 1];

if (jsrMode && jsrMode !== "src" && jsrMode !== "dist") {
	console.error(`❌ unknown --jsr mode "${jsrMode}" (expected "src" or "dist")`);
	process.exit(1);
}

if (jsrMode) {
	writeJsrConfig(jsrMode as "src" | "dist");
}

function writeJsrConfig(mode: "src" | "dist") {
	console.log(`✍️  Generating jsr.json (${mode})...`);

	const exports: Record<string, string> = {};
	for (const [key, val] of Object.entries(srcExports)) {
		if (typeof val !== "string") continue;
		// CSS exports are dev-only and not published, same as the npm package.
		if (val.endsWith(".css")) continue;
		// rewritePath turns "./src/index.ts" into "./index.js".
		exports[key] = mode === "src" ? val : `./dist/${rewritePath(val, "js").slice(2)}`;
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

	if (mode === "dist") injectSelfTypes();

	// dist is gitignored, so dist mode un-ignores it with a "!" negation; JSR
	// honors .gitignore otherwise and would drop the whole build from the graph.
	const publish =
		mode === "src"
			? { include: ["src", "README.md", "LICENSE*"], exclude: ["**/*.test.ts"] }
			: { include: ["dist", "README.md", "LICENSE*"], exclude: ["!dist"] };

	const jsr = {
		name: pkg.name,
		version: pkg.version,
		...(pkg.license ? { license: pkg.license } : {}),
		exports,
		...(Object.keys(imports).length ? { imports } : {}),
		publish,
	};

	writeFileSync("jsr.json", JSON.stringify(jsr, null, 2));
	console.log("📦 jsr.json written");
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
		writeFileSync(js, `/* @ts-self-types="./${basename(dts)}" */\n${body}`);
	}
}

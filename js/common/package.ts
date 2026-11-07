// Script to build and package a workspace for distribution
// This creates a dist/ folder with the correct paths and dependencies for publishing
// Split from release.ts to allow building packages without publishing

import { copyFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { publint } from "publint";
import { formatMessage } from "publint/utils";
import { checkReports } from "./api.ts";
import { problems } from "./declarations.ts";

console.log("✍️  Rewriting package.json...");
const pkg = JSON.parse(readFileSync("package.json", "utf8"));

// Capture whether the package publishes before pkg.scripts is cleared below.
const published = Boolean(pkg.scripts?.release);

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

// npm normalizes bin targets to drop the leading "./" and warns when it has to.
function rewriteBin(p: string): string {
	return rewritePath(p, "js").replace(/^\.\//, "");
}

if (pkg.bin) {
	if (typeof pkg.bin === "string") {
		pkg.bin = rewriteBin(pkg.bin);
	} else if (typeof pkg.bin === "object") {
		for (const key in pkg.bin) {
			pkg.bin[key] = rewriteBin(pkg.bin[key]);
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
rewriteWorkspaceDependency(pkg.peerDependencies);

pkg.devDependencies = undefined;
pkg.scripts = undefined;

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

console.log("🔍 Checking declaration imports...");
const declarations = new Map<string, string>();
for (const rel of new Bun.Glob("**/*.d.ts").scanSync("dist")) {
	const file = resolve("dist", rel);
	declarations.set(file, readFileSync(file, "utf8"));
}
const unresolved = problems(resolve("dist"), declarations);
if (unresolved.length > 0) {
	for (const problem of unresolved) console.error(problem);
	process.exit(1);
}

if (published) {
	console.log("🔍 Checking API reports...");
	checkReports(".");
}

console.log("📦 Package built successfully in dist/");

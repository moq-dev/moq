// Script to build and package a workspace for distribution
// This creates a dist/ folder with the correct paths and dependencies for publishing
// Split from release.ts to allow building packages without publishing

import { copyFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

console.log("✍️  Rewriting package.json...");
const pkg = JSON.parse(readFileSync("package.json", "utf8"));

function rewritePath(p: string): string {
	return p.replace(/^\.\/src/, ".").replace(/\.ts(x)?$/, ".js");
}

pkg.main &&= rewritePath(pkg.main);
pkg.types &&= rewritePath(pkg.types);

if (pkg.exports) {
	for (const key in pkg.exports) {
		const val = pkg.exports[key];
		if (typeof val === "string") {
			pkg.exports[key] = rewritePath(val);
		} else if (typeof val === "object") {
			for (const sub in val) {
				if (typeof val[sub] === "string") {
					val[sub] = rewritePath(val[sub]);
				}
			}
		}
	}
}

if (pkg.sideEffects) {
	pkg.sideEffects = pkg.sideEffects.map(rewritePath);
}

if (pkg.files) {
	pkg.files = pkg.files.map(rewritePath);
}

if (pkg.bin) {
	if (typeof pkg.bin === "string") {
		pkg.bin = rewritePath(pkg.bin);
	} else if (typeof pkg.bin === "object") {
		for (const key in pkg.bin) {
			pkg.bin[key] = rewritePath(pkg.bin[key]);
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

// Write the rewritten package.json
writeFileSync("dist/package.json", JSON.stringify(pkg, null, 2));

// Copy static files
console.log("📄 Copying README.md...");
copyFileSync("README.md", join("dist", "README.md"));

console.log("📦 Package built successfully in dist/");

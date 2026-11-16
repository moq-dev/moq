// Script to build and package a workspace for distribution
// This creates a dist/ folder with the correct paths and dependencies for publishing
// Split from release.ts to allow building packages without publishing

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { publint } from "publint";
import { formatMessage } from "publint/utils";
import { checkPackageConstructors } from "./constructors.ts";
import { specifiers } from "./deps.ts";

checkPackageConstructors(process.cwd());

console.log("✍️  Rewriting package.json...");
const pkg = JSON.parse(readFileSync("package.json", "utf8"));

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

// Worklets and workers that a page whose CSP refuses blob: hosts itself; see the package's assets().
if (existsSync("dist/assets")) {
	pkg.exports["./assets/*"] = "./assets/*";
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

// The directory of a workspace package, named without its scope.
function workspaceDir(name: string): string {
	return `../${name.includes("/") ? name.split("/")[1] : name}`;
}

function rewriteWorkspaceDependency(dependencies?: Record<string, string>) {
	if (!dependencies) return;
	for (const [name, version] of Object.entries(dependencies)) {
		if (typeof version === "string" && version.startsWith("workspace:")) {
			// Read the actual version from the workspace package
			const workspacePkg = JSON.parse(readFileSync(`${workspaceDir(name)}/package.json`, "utf8"));
			if (workspacePkg.private && !pkg.private) {
				console.error(`${pkg.name} depends on the private ${name}; bundle it from devDependencies instead`);
				process.exit(1);
			}
			dependencies[name] = `^${workspacePkg.version}`;
			console.log(`🔗 Converted ${name}: ${version} → ^${workspacePkg.version}`);
		}
	}
}

// Convert workspace dependencies to published versions
rewriteWorkspaceDependency(pkg.dependencies);
rewriteWorkspaceDependency(pkg.peerDependencies);

function files(dir: string): string[] {
	return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
		const path = join(dir, entry.name);
		return entry.isDirectory() ? files(path) : [path];
	});
}

// A private workspace package ships inside each published package that imports it: Vite bundles
// its code and stylesheets, and here its declarations are emitted to dist/bundled/<dir>, with each
// import of it in this package's declarations pointed at that copy.
for (const [name, version] of Object.entries<string>(pkg.devDependencies ?? {})) {
	if (!version.startsWith("workspace:")) continue;
	const source = workspaceDir(name);
	const bundled = JSON.parse(readFileSync(`${source}/package.json`, "utf8"));
	if (!bundled.private) continue;

	const imports = (file: string) =>
		specifiers(readFileSync(file, "utf8")).some((s) => s === name || s.startsWith(`${name}/`));
	const importers = files("dist").filter((file) => file.endsWith(".d.ts") && imports(file));
	if (importers.length === 0) continue;

	const out = join("dist", "bundled", source.slice("../".length));
	console.log(`📎 Bundling the declarations of ${name} into ${out}...`);
	const tsc = ["x", "--no-install", "tsc", "-p", `${source}/tsconfig.build.json`, "--outDir", out];
	execFileSync(process.execPath, tsc, { stdio: "inherit" });
	const literal = new RegExp(`(["'])${name}(/[^"']*)?\\1`, "g");
	for (const file of importers) {
		const body = readFileSync(file, "utf8").replace(literal, (_, quote: string, subpath = "") => {
			const target = bundled.exports?.[`.${subpath}`];
			if (typeof target !== "string" || !target.endsWith(".ts")) {
				console.error(`${file}: imports ${name}${subpath}, which has no such code export`);
				process.exit(1);
			}
			const path = relative(dirname(file), join(out, rewritePath(target, "js")));
			return `${quote}${path.startsWith(".") ? path : `./${path}`}${quote}`;
		});
		writeFileSync(file, body);
	}
}

// What ships imports no @moq package it does not depend on, such as a private one left unbundled.
const declared = new Set([
	pkg.name,
	...Object.keys(pkg.dependencies ?? {}),
	...Object.keys(pkg.peerDependencies ?? {}),
]);
for (const file of files("dist")) {
	if (!file.endsWith(".js") && !file.endsWith(".d.ts")) continue;
	for (const specifier of specifiers(readFileSync(file, "utf8"))) {
		const [scope, base] = specifier.split("/");
		if (scope === "@moq" && base && !declared.has(`${scope}/${base}`)) {
			console.error(`${file}: imports ${specifier}, which ${pkg.name} does not depend on`);
			process.exit(1);
		}
	}
}

pkg.devDependencies = undefined;
pkg.scripts = undefined;

// Write the rewritten package.json
writeFileSync("dist/package.json", JSON.stringify(pkg, null, 2));

// A package that reports its own version keeps it in src/version.ts as "dev", since the
// workspace runs src/ unbuilt; the published build gets the real one.
if (existsSync("src/version.ts")) {
	console.log(`🏷️  Stamping version ${pkg.version}...`);
	const file = join("dist", "version.js");
	const body = readFileSync(file, "utf8");
	const placeholder = 'VERSION = "dev";';
	if (body.split(placeholder).length !== 2) {
		console.error(`${file} must contain exactly one \`${placeholder}\``);
		process.exit(1);
	}
	writeFileSync(file, body.replace(placeholder, `VERSION = ${JSON.stringify(pkg.version)};`));
}

// Copy static files, including the license and patent notices of any third-party code ported in,
// named like `<UPSTREAM>-LICENSE` and `<UPSTREAM>-PATENTS`.
console.log("📄 Copying README.md...");
copyFileSync("README.md", join("dist", "README.md"));
for (const notice of readdirSync(".").filter((name) => /^[A-Z0-9]+-(LICENSE|PATENTS)$/.test(name))) {
	console.log(`📄 Copying ${notice}...`);
	copyFileSync(notice, join("dist", notice));
}

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

import { execSync } from "node:child_process";

const dryRun = process.argv.includes("--dry-run") || process.env.DRY_RUN === "true";

// Read package.json to get name and version
const pkg = JSON.parse(await Bun.file("package.json").text());
const { name, version } = pkg;

// Skip the already-published check in dry-run mode so we always exercise
// the build + publish manifest, even when the version is already on npm.
if (!dryRun) {
	let published = "0.0.0";
	try {
		published = execSync(`npm view ${name} version`, {
			encoding: "utf8",
			stdio: ["pipe", "pipe", "pipe"],
		}).trim();
	} catch {
		// Package not published yet
	}

	if (version === published) {
		console.log(`⏭️  ${name}@${version} already published, skipping`);
		process.exit(0);
	}
}

console.log(`📦 Building ${name}@${version}...`);
execSync("bun run build", { stdio: "inherit" });

const suffix = dryRun ? " (dry-run)" : "";
console.log(`🚀 Publishing ${name}@${version}${suffix}...`);
// Use npm for publishing to support OIDC trusted publishing
const publishCmd = dryRun ? "npm publish --access public --dry-run" : "npm publish --access public";
execSync(publishCmd, {
	stdio: "inherit",
	cwd: "dist",
});

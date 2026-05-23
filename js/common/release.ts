import { execFileSync } from "node:child_process";

const dryRun = process.argv.includes("--dry-run") || process.env.DRY_RUN === "true";

// Read package.json to get name and version
const pkg = JSON.parse(await Bun.file("package.json").text());
const { name, version } = pkg;

// Skip the already-published check in dry-run mode so we always exercise
// the build + publish manifest, even when the version is already on npm.
if (!dryRun) {
	let published = "0.0.0";
	try {
		published = execFileSync("npm", ["view", name, "version"], {
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
execFileSync("bun", ["run", "build"], { stdio: "inherit" });

if (dryRun) {
	// `npm publish --dry-run` still hits the registry to check for version
	// conflicts and errors out when the version is already published (which
	// is the common case on PRs of main). `npm pack --dry-run` exercises the
	// same packaging path without any registry roundtrip.
	console.log(`🧪 Packing ${name}@${version} (dry-run)...`);
	execFileSync("npm", ["pack", "--dry-run"], { stdio: "inherit", cwd: "dist" });
} else {
	console.log(`🚀 Publishing ${name}@${version}...`);
	// Use npm for publishing to support OIDC trusted publishing
	execFileSync("npm", ["publish", "--access", "public"], { stdio: "inherit", cwd: "dist" });
}

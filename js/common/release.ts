import { execFileSync } from "node:child_process";

const dryRun = process.argv.includes("--dry-run") || process.env.DRY_RUN === "true";

// Read package.json to get the name and version.
const { name, version } = JSON.parse(await Bun.file("package.json").text());

function npmHasVersion(): boolean {
	try {
		const published = execFileSync("npm", ["view", name, "version"], {
			encoding: "utf8",
			stdio: ["pipe", "pipe", "pipe"],
		}).trim();
		return published === version;
	} catch {
		return false; // not published yet
	}
}

// Skip the registry check in dry-run mode so we always exercise the build and
// pack, even when the version is already published.
if (!dryRun && npmHasVersion()) {
	console.log(`⏭️  ${name}@${version} already published, skipping`);
	process.exit(0);
}

console.log(`📦 Building ${name}@${version}...`);
execFileSync("bun", ["run", "build"], { stdio: "inherit" });

// npm
if (dryRun) {
	// `npm publish --dry-run` still hits the registry to check for version
	// conflicts and errors out when the version is already published (which
	// is the common case on PRs of main). `npm pack --dry-run` exercises the
	// same packaging path without any registry roundtrip.
	console.log(`🧪 Packing ${name}@${version} (dry-run)...`);
	execFileSync("npm", ["pack", "--dry-run"], { stdio: "inherit", cwd: "dist" });
} else {
	console.log(`🚀 Publishing ${name}@${version} to npm...`);
	// Use npm for publishing to support OIDC trusted publishing
	execFileSync("npm", ["publish", "--access", "public"], { stdio: "inherit", cwd: "dist" });
}

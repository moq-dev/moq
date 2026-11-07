import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";

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

// Dry runs may use a releasable workspace version; real publishes require npm.
const siblings = new Map<string, string>();
if (dryRun) {
	for (let root = process.cwd(); ; root = dirname(root)) {
		const file = Bun.file(join(root, "package.json"));
		if (await file.exists()) {
			const workspace = await file.json();
			if (workspace.workspaces) {
				for (const path of workspace.workspaces as string[]) {
					// Match the literal workspace paths used by the release runner.
					if (/[*?[\]{}!]/.test(path)) throw new Error(`Unsupported workspace pattern: ${path}`);
					const sibling = await Bun.file(join(root, path, "package.json")).json();
					if (!sibling.private && sibling.scripts?.release) siblings.set(sibling.name, sibling.version);
				}
				break;
			}
		}
		if (dirname(root) === root) break;
	}
}
const manifest = JSON.parse(await Bun.file("dist/package.json").text()) as {
	dependencies?: Record<string, string>;
	peerDependencies?: Record<string, string>;
};
checkDependencies(
	[...Object.entries(manifest.dependencies ?? {}), ...Object.entries(manifest.peerDependencies ?? {})]
		.filter(([name]) => name.startsWith("@moq/"))
		.map(([name, range]) => `${name}@${range}`),
);

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

function checkDependencies(specs: string[]) {
	const missing = [...new Set(specs)].filter((spec) => {
		let found = false;
		try {
			const output = execFileSync("npm", ["view", spec, "version", "--json"], {
				encoding: "utf8",
				stdio: ["ignore", "pipe", "pipe"],
			}).trim();
			const versions = output ? JSON.parse(output) : [];
			found = typeof versions === "string" ? versions.length > 0 : Array.isArray(versions) && versions.length > 0;
		} catch (error) {
			const command = error as { stdout?: string; stderr?: string };
			// Only a missing version can be supplied by this release. A broken registry
			// must not masquerade as an unpublished sibling and give the gate a pass.
			let code: string | undefined;
			try {
				code = JSON.parse(command.stdout ?? "").error?.code;
			} catch {
				/* Preserve the original failure below. */
			}
			if (code !== "E404")
				throw new Error(`Cannot resolve ${spec}: ${command.stderr ?? error}`, { cause: error });
		}
		return !found;
	});
	const unresolved = missing.filter((spec) => {
		const split = spec.lastIndexOf("@");
		const version = siblings.get(spec.slice(0, split));
		return !dryRun || !version || !Bun.semver.satisfies(version, spec.slice(split + 1));
	});
	if (unresolved.length) throw new Error(`Cannot publish: unresolved dependencies: ${unresolved.join(", ")}`);
}

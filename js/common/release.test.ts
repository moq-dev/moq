import { expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const release = join(import.meta.dir, "release.ts");

type Manifest = {
	dependencies?: Record<string, string>;
	peerDependencies?: Record<string, string>;
};
type Reply = { status?: number; output?: string; error?: string };

// Run the real release entrypoint with isolated command substitutes: no registry
// credentials, builds, or publishing escape the fixture.
function run(options: {
	manifest: Manifest;
	imports?: Record<string, string>;
	nested?: boolean;
	replies?: Record<string, Reply>;
	dryRun?: boolean;
	jsr?: boolean;
	published?: boolean;
	jsrToken?: string;
	sibling?: { name: string; version: string; private?: boolean; scripts?: { release: string } };
}) {
	const cwd = mkdtempSync(join(tmpdir(), "moq-release-"));
	try {
		const bin = join(cwd, "bin");
		mkdirSync(bin);
		if (options.sibling) {
			mkdirSync(join(cwd, "sibling"));
			writeFileSync(
				join(cwd, "sibling", "package.json"),
				JSON.stringify({ scripts: { release: "unused" }, ...options.sibling }),
			);
		}
		writeFileSync(
			join(cwd, "package.json"),
			JSON.stringify({
				name: "@moq/fixture",
				version: "1.0.0",
				jsr: options.jsr ?? false,
				scripts: { release: "unused" },
				workspaces: options.sibling ? ["sibling"] : undefined,
				dependencies: { "@moq/source-only": "workspace:^" },
			}),
		);
		let packageDir = cwd;
		if (options.nested) {
			packageDir = join(cwd, "packages", "fixture");
			mkdirSync(packageDir, { recursive: true });
			const manifest = JSON.parse(readFileSync(join(cwd, "package.json"), "utf8"));
			delete manifest.workspaces;
			writeFileSync(join(packageDir, "package.json"), JSON.stringify(manifest));
		}
		writeFileSync(join(cwd, "fixture.json"), JSON.stringify(options));
		for (const tool of ["bun", "npm", "deno"]) {
			writeFileSync(
				join(bin, tool),
				`#!${process.execPath}
import { appendFileSync, readFileSync, mkdirSync, writeFileSync } from "node:fs";
const options = JSON.parse(readFileSync("${cwd}/fixture.json", "utf8"));
const args = process.argv.slice(2);
appendFileSync("${cwd}/calls.jsonl", JSON.stringify(["${tool}", ...args]) + "\\n");
if (process.env.JSR_TOKEN) appendFileSync("${cwd}/leaks.txt", "${tool}\\n");
if ("${tool}" === "bun") {
 mkdirSync("dist", { recursive: true });
 writeFileSync("dist/package.json", JSON.stringify(options.manifest));
 const deps = { ...options.manifest.dependencies, ...options.manifest.peerDependencies };
 writeFileSync("jsr.json", JSON.stringify({ imports: options.imports ?? Object.fromEntries(Object.entries(deps).map(([k,v]) => [k, "npm:" + k + "@" + v])) }));
} else if ("${tool}" === "npm" && args[0] === "view") {
 if (args[1] === "@moq/fixture") { console.log(options.published ? "1.0.0" : "0.9.0"); }
 else {
 const r = options.replies?.[args[1]] ?? { status: 1, error: "E404: version not found" };
 if (args.includes("--json")) {
 if (r.output) console.log(JSON.stringify(r.output.split("\\n")));
 else if (r.status) console.log(JSON.stringify({ error: { code: r.error?.startsWith("E404") ? "E404" : "ECONNRESET" } }));
} else if (r.output) console.log(r.output);
 if (r.error) console.error(r.error);
 process.exit(r.status ?? 0);
 }
}
`,
				{ mode: 0o755 },
			);
		}
		const result = Bun.spawnSync([process.execPath, release, ...(options.dryRun ? ["--dry-run"] : [])], {
			cwd: packageDir,
			env: {
				PATH: bin,
				HOME: cwd,
				DRY_RUN: String(options.dryRun ?? false),
				...(options.jsrToken ? { JSR_TOKEN: options.jsrToken } : {}),
			},
			stdout: "pipe",
			stderr: "pipe",
		});
		const calls = readFileSync(join(cwd, "calls.jsonl"), "utf8")
			.trim()
			.split("\n")
			.map((s) => JSON.parse(s) as string[]);
		const leaks = existsSync(join(cwd, "leaks.txt"))
			? readFileSync(join(cwd, "leaks.txt"), "utf8").trim().split("\n")
			: [];
		return { code: result.exitCode, output: result.stdout.toString() + result.stderr.toString(), calls, leaks };
	} finally {
		rmSync(cwd, { recursive: true, force: true });
	}
}

for (const jsr of [false, true]) {
	test(`dry-run packs an unpublished workspace dependency without publishing (jsr=${jsr})`, () => {
		const result = run({
			dryRun: true,
			jsr,
			sibling: { name: "@moq/net", version: "999.0.0" },
			manifest: { dependencies: { "@moq/net": "^999.0.0" } },
		});
		expect(result.code).toBe(0);
		expect(result.calls).toContainEqual(["npm", "pack", "--dry-run"]);
		expect(result.calls.some((c) => c[1] === "publish")).toBe(false);
		if (jsr) expect(result.output).toContain("Skipping JSR dry-run");
	});
}

test("refuses all missing dependencies and peers before publishing", () => {
	const result = run({
		manifest: {
			dependencies: { "@moq/net": "^999.0.0", external: "1" },
			peerDependencies: { "@moq/signals": "^998.0.0" },
		},
	});
	expect(result.code).not.toBe(0);
	expect(result.output).toContain("@moq/net@^999.0.0");
	expect(result.output).toContain("@moq/signals@^998.0.0");
	expect(result.calls.some(([tool, cmd]) => tool === "npm" && ["pack", "publish"].includes(cmd))).toBe(false);
});

test("checks built ranges, both constraints on a shared dependency, and not external packages", () => {
	const result = run({
		manifest: {
			dependencies: { "@moq/net": "^1.0.0", external: "1" },
			peerDependencies: { "@moq/net": "^2.0.0" },
		},
		replies: { "@moq/net@^1.0.0": { output: "1.2.0" } },
	});
	expect(result.code).not.toBe(0);
	expect(result.output).toContain("@moq/net@^2.0.0");
	expect(result.calls.filter(([tool, cmd]) => tool === "npm" && cmd === "view").map((c) => c[2])).toEqual([
		"@moq/fixture",
		"@moq/net@^1.0.0",
		"@moq/net@^2.0.0",
	]);
});

test("an empty successful registry response does not establish a matching version", () => {
	const result = run({
		manifest: { dependencies: { "@moq/net": "^9" } },
		replies: { "@moq/net@^9": {} },
	});
	expect(result.code).not.toBe(0);
});

test("registry errors prevent publishing and preserve the diagnostic", () => {
	const result = run({
		manifest: { dependencies: { "@moq/net": "^1" } },
		replies: {
			"@moq/net@^1": { status: 1, error: "ECONNRESET registry unavailable" },
		},
	});
	expect(result.code).not.toBe(0);
	expect(result.output).toContain("ECONNRESET");
	expect(result.calls.some((c) => c[1] === "publish")).toBe(false);
});

test("matching dependencies publish, deduplicating identical dependency and peer ranges", () => {
	const result = run({
		manifest: { dependencies: { "@moq/net": "^1" }, peerDependencies: { "@moq/net": "^1" } },
		replies: {
			"@moq/net@^1": { output: "1.1.0\n1.2.0" },
		},
	});
	expect(result.code).toBe(0);
	expect(result.calls.filter((c) => c[1] === "view" && c[2] === "@moq/net@^1")).toHaveLength(1);
	expect(result.calls.at(-1)).toEqual(["npm", "publish", "--access", "public"]);
});

test("JSR dry-run checks its import map once and runs when dependencies are published", () => {
	const result = run({
		dryRun: true,
		jsr: true,
		manifest: { dependencies: { "@moq/net": "^1" } },
		replies: {
			"@moq/net@^1": { output: "1.1.0" },
		},
	});
	expect(result.code).toBe(0);
	expect(result.calls.filter((c) => c[1] === "view")).toEqual([["npm", "view", "@moq/net@^1", "version", "--json"]]);
	expect(result.calls.some((c) => c[0] === "deno" && c.includes("--dry-run"))).toBe(true);
	expect(result.calls.some((c) => c[0] === "npm" && c[1] === "publish")).toBe(false);
});

test("the JSR token reaches only deno publish", () => {
	const result = run({ jsr: true, jsrToken: "jsr-secret", manifest: {} });
	expect(result.code).toBe(0);
	expect(result.calls.at(-1)).toEqual([
		"deno",
		"publish",
		"--unstable-bare-node-builtins",
		"--unstable-sloppy-imports",
		"--no-check",
		"--allow-dirty",
		"--token",
		"jsr-secret",
	]);
	expect(result.calls.filter((c) => c.includes("jsr-secret"))).toHaveLength(1);
	expect(result.leaks).toEqual([]);
});

test("a local sibling cannot satisfy the registry check for a real publish", () => {
	const result = run({
		sibling: { name: "@moq/net", version: "999.0.0" },
		manifest: { dependencies: { "@moq/net": "^999.0.0" } },
	});
	expect(result.code).not.toBe(0);
	expect(result.output).toContain("@moq/net@^999.0.0");
	expect(result.calls.some((c) => c[1] === "publish")).toBe(false);
});

test("packages without internal dependencies still pack", () => {
	const result = run({ dryRun: true, manifest: {} });
	expect(result.code).toBe(0);
	expect(result.calls.at(-1)).toEqual(["npm", "pack", "--dry-run"]);
});

test("an already-published npm-only version keeps the existing skip", () => {
	const result = run({ published: true, manifest: { dependencies: { "@moq/net": "^999" } } });
	expect(result.code).toBe(0);
	expect(result.calls).toEqual([["npm", "view", "@moq/fixture", "version"]]);
});

for (const jsr of [false, true]) {
	for (const sibling of [undefined, { name: "@moq/net", version: "999.0.0" }]) {
		test(`dry-run rejects an unresolved non-sibling (jsr=${jsr}, sibling=${Boolean(sibling)})`, () => {
			const result = run({
				dryRun: true,
				jsr,
				sibling,
				manifest: { dependencies: { "@moq/ghost": "^999", ...(sibling ? { "@moq/net": "^999" } : {}) } },
			});
			expect(result.code).not.toBe(0);
			expect(result.output).toContain("@moq/ghost@^999");
			expect(result.calls.some((c) => c[1] === "pack" || c[1] === "publish")).toBe(false);
		});
	}
}
for (const sibling of [
	{ name: "@moq/net", version: "1.0.0" },
	{ name: "@moq/net", version: "999.0.0", private: true },
	{ name: "@moq/net", version: "999.0.0", scripts: undefined },
]) {
	test(`dry-run rejects a sibling that cannot supply the range: ${JSON.stringify(sibling)}`, () => {
		const result = run({ dryRun: true, sibling, manifest: { peerDependencies: { "@moq/net": "^999" } } });
		expect(result.code).not.toBe(0);
		expect(result.output).toContain("@moq/net@^999");
		expect(result.calls.some((c) => c[1] === "pack" || c[1] === "publish")).toBe(false);
	});
}

test("dry-run cannot hide a registry failure behind a local sibling", () => {
	const result = run({
		dryRun: true,
		sibling: { name: "@moq/net", version: "1.0.0" },
		manifest: { dependencies: { "@moq/net": "^1" } },
		replies: { "@moq/net@^1": { status: 1, error: "ECONNRESET registry unavailable" } },
	});
	expect(result.code).not.toBe(0);
	expect(result.output).toContain("ECONNRESET");
	expect(result.calls.some((c) => c[1] === "pack" || c[1] === "publish")).toBe(false);
});

test("dry-run finds releasable siblings from a nested package directory", () => {
	const result = run({
		dryRun: true,
		nested: true,
		sibling: { name: "@moq/net", version: "2.0.0" },
		manifest: { dependencies: { "@moq/net": "^2" } },
	});
	expect(result.code).toBe(0);
	expect(result.calls).toContainEqual(["npm", "pack", "--dry-run"]);
});

test("a registry version can satisfy a range even when the local sibling cannot", () => {
	const result = run({
		dryRun: true,
		sibling: { name: "@moq/net", version: "1.0.0" },
		manifest: { dependencies: { "@moq/net": "^2" } },
		replies: { "@moq/net@^2": { output: "2.0.0" } },
	});
	expect(result.code).toBe(0);
	expect(result.calls).toContainEqual(["npm", "pack", "--dry-run"]);
});

test("an unresolved JSR-only non-sibling fails before packing", () => {
	const result = run({ dryRun: true, jsr: true, manifest: {}, imports: { "@moq/ghost": "npm:@moq/ghost@^999" } });
	expect(result.code).not.toBe(0);
	expect(result.output).toContain("@moq/ghost@^999");
	expect(result.calls.some((c) => c[1] === "pack" || c[1] === "publish")).toBe(false);
});

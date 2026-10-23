import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
	replies?: Record<string, Reply>;
	dryRun?: boolean;
	jsr?: boolean;
	published?: boolean;
}) {
	const cwd = mkdtempSync(join(tmpdir(), "moq-release-"));
	try {
		const bin = join(cwd, "bin");
		mkdirSync(bin);
		writeFileSync(
			join(cwd, "package.json"),
			JSON.stringify({
				name: "@moq/fixture",
				version: "1.0.0",
				jsr: options.jsr ?? false,
				scripts: { release: "unused" },
				dependencies: { "@moq/source-only": "workspace:^" },
			}),
		);
		writeFileSync(join(cwd, "fixture.json"), JSON.stringify(options));
		for (const tool of ["bun", "npm", "deno"]) {
			writeFileSync(
				join(bin, tool),
				`#!${process.execPath}
import { appendFileSync, readFileSync, mkdirSync, writeFileSync } from "node:fs";
const options = JSON.parse(readFileSync("${cwd}/fixture.json", "utf8"));
const args = process.argv.slice(2);
appendFileSync("${cwd}/calls.jsonl", JSON.stringify(["${tool}", ...args]) + "\\n");
if ("${tool}" === "bun") {
 mkdirSync("dist", { recursive: true });
 writeFileSync("dist/package.json", JSON.stringify(options.manifest));
 const deps = { ...options.manifest.dependencies, ...options.manifest.peerDependencies };
 writeFileSync("jsr.json", JSON.stringify({ imports: Object.fromEntries(Object.entries(deps).map(([k,v]) => [k, "npm:" + k + "@" + v])) }));
} else if ("${tool}" === "npm" && args[0] === "view") {
 if (args[1] === "@moq/fixture") { console.log(options.published ? "1.0.0" : "0.9.0"); }
 else {
 const r = options.replies?.[args[1]] ?? { status: 1, error: "E404: version not found" };
 if (r.output) console.log(r.output);
 if (r.error) console.error(r.error);
 process.exit(r.status ?? 0);
 }
}
`,
				{ mode: 0o755 },
			);
		}
		const result = Bun.spawnSync([process.execPath, release, ...(options.dryRun ? ["--dry-run"] : [])], {
			cwd,
			env: { PATH: bin, HOME: cwd, DRY_RUN: String(options.dryRun ?? false) },
			stdout: "pipe",
			stderr: "pipe",
		});
		const calls = readFileSync(join(cwd, "calls.jsonl"), "utf8")
			.trim()
			.split("\n")
			.map((s) => JSON.parse(s) as string[]);
		return { code: result.exitCode, output: result.stdout.toString() + result.stderr.toString(), calls };
	} finally {
		rmSync(cwd, { recursive: true, force: true });
	}
}

for (const dryRun of [false, true]) {
	test(`refuses all missing dependencies and peers before publishing (dry-run=${dryRun})`, () => {
		const result = run({
			dryRun,
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
}

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
		dryRun: true,
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

test("JSR dry-run reuses the verified dependency graph without querying it again", () => {
	const result = run({
		dryRun: true,
		jsr: true,
		manifest: { dependencies: { "@moq/net": "^1" } },
		replies: {
			"@moq/net@^1": { output: "1.1.0" },
		},
	});
	expect(result.code).toBe(0);
	expect(result.calls.filter((c) => c[1] === "view")).toEqual([["npm", "view", "@moq/net@^1", "version"]]);
	expect(result.calls.some((c) => c[0] === "deno" && c.includes("--dry-run"))).toBe(true);
	expect(result.calls.some((c) => c[0] === "npm" && c[1] === "publish")).toBe(false);
});

test("an unresolved dependency fails a JSR-enabled dry-run instead of skipping it", () => {
	const result = run({
		dryRun: true,
		jsr: true,
		manifest: { dependencies: { "@moq/net": "^999" } },
	});
	expect(result.code).not.toBe(0);
	expect(result.output).toContain("@moq/net@^999");
	expect(result.calls.some((c) => c[0] === "deno" || c[1] === "pack")).toBe(false);
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

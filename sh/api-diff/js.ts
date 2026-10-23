// Write an API Extractor report for every entrypoint of each published `@moq/*`
// package, once for its newest npm release and once for the built dist/, so
// sh/api-diff/run.sh can diff them. Usage: bun sh/api-diff/js.ts <out>
//
// <out>/{baseline,current}/<package>/<entry>.api.md holds the reports, and
// <out>/versions.tsv names each package's baseline version, or "-" when it was
// never published. Exits 1 after writing everything if any entrypoint failed.

import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Extractor, ExtractorConfig, ExtractorLogLevel } from "@microsoft/api-extractor";

const out = resolve(process.argv[2] ?? "");
if (!process.argv[2]) throw new Error("usage: bun sh/api-diff/js.ts <out>");

// Both sides are analyzed outside the repository, where no node_modules
// resolves their imports: the npm tarball ships without its dependencies, so the
// built side must not see them either, or the reports differ for that alone.
const work = mkdtempSync(join(tmpdir(), "api-diff-"));
const versions: string[] = [];

try {
	for (const manifest of new Bun.Glob("js/*/package.json").scanSync()) {
		const pkg = JSON.parse(readFileSync(manifest, "utf8"));
		if (!pkg.scripts?.release) continue;
		const name: string = pkg.name;

		const baseline = published(name);
		versions.push(`${name}\t${baseline ?? "-"}`);
		if (!baseline) continue;

		const dist = join(manifest, "..", "dist");
		if (!existsSync(join(dist, "package.json"))) throw new Error(`${dist} is not built; run \`just js build\``);
		const current = join(work, "current", name);
		cpSync(dist, current, { recursive: true });
		report(current, join(out, "current", name));

		const tarballs = join(work, "tarballs");
		mkdirSync(tarballs, { recursive: true });
		const pack = ["pack", `${name}@${baseline}`, "--silent", "--pack-destination", tarballs];
		const tarball = execFileSync("npm", pack, { encoding: "utf8" }).trim();
		const extracted = join(work, "baseline", name);
		mkdirSync(extracted, { recursive: true });
		execFileSync("tar", ["-xzf", join(tarballs, tarball), "-C", extracted, "--strip-components=1"]);
		report(extracted, join(out, "baseline", name));
	}
} finally {
	rmSync(work, { recursive: true, force: true });
}

mkdirSync(out, { recursive: true });
writeFileSync(join(out, "versions.tsv"), `${versions.join("\n")}\n`);

// The newest npm release of a package, or undefined if it was never published.
function published(name: string): string | undefined {
	try {
		return execFileSync("npm", ["view", name, "version"], { encoding: "utf8", stdio: "pipe" }).trim();
	} catch (err) {
		const stderr = String((err as { stderr?: unknown }).stderr ?? "");
		if (stderr.includes("E404")) return undefined;
		throw err;
	}
}

// One report per entrypoint with declarations, named after its subpath.
function report(root: string, dest: string) {
	mkdirSync(dest, { recursive: true });
	rewriteExternalNamespaces(root);

	const exports: Record<string, unknown> = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).exports ?? {};
	for (const [subpath, target] of Object.entries(exports)) {
		const types = (target as { types?: string }).types;
		if (!types) continue;
		const entry = subpath === "." ? "index" : subpath.replace(/^\.\//, "").replaceAll("/", "-");
		extract(root, join(root, types), dest, entry);
	}
}

// API Extractor cannot follow `export * as X from "<package>"`, which every
// entrypoint uses to re-export its dependencies, but reads the equivalent
// import-then-export form. Both sides are rewritten alike.
function rewriteExternalNamespaces(root: string) {
	for (const rel of new Bun.Glob("**/*.d.ts").scanSync(root)) {
		const file = join(root, rel);
		const body = readFileSync(file, "utf8");
		const next = body.replace(
			/^export \* as ([A-Za-z_$][\w$]*) from "([^."][^"]*)";$/gm,
			'import * as $1 from "$2";\nexport { $1 };',
		);
		if (next !== body) writeFileSync(file, next);
	}
}

function extract(root: string, entrypoint: string, dest: string, entry: string) {
	const config = ExtractorConfig.prepare({
		configObject: {
			projectFolder: root,
			mainEntryPointFilePath: entrypoint,
			compiler: {
				overrideTsconfig: {
					compilerOptions: {
						target: "esnext",
						module: "esnext",
						moduleResolution: "bundler",
						lib: ["esnext", "dom"],
						strict: true,
					},
					files: [entrypoint],
				},
			},
			apiReport: {
				enabled: true,
				reportFolder: dest,
				reportTempFolder: join(root, ".api-extractor"),
				reportFileName: entry,
			},
			docModel: { enabled: false },
			dtsRollup: { enabled: false },
			tsdocMetadata: { enabled: false },
			// The report is the output; its findings about doc comments and
			// unresolved dependencies are the same on both sides.
			messages: {
				compilerMessageReporting: { default: { logLevel: ExtractorLogLevel.None } },
				extractorMessageReporting: { default: { logLevel: ExtractorLogLevel.None } },
				tsdocMessageReporting: { default: { logLevel: ExtractorLogLevel.None } },
			},
		},
		configObjectFullPath: join(root, "api-extractor.json"),
		packageJsonFullPath: join(root, "package.json"),
	});
	// A declaration API Extractor cannot analyze stands in for that entrypoint's
	// report, so the diff shows it and the run still covers every other package.
	try {
		// localBuild writes the report instead of failing because it changed.
		const result = Extractor.invoke(config, { localBuild: true });
		if (!result.succeeded) throw new Error(`${result.errorCount} errors`);

		// Keep only the API: whether an item has a doc comment is not part of
		// it, and every exported item is @public.
		const file = join(dest, `${entry}.api.md`);
		const body = readFileSync(file, "utf8")
			.replaceAll(" (undocumented)", "")
			.replace(/^ *\/\/( @public| \(No @packageDocumentation comment for this package\))?\n/gm, "");
		writeFileSync(file, body);
	} catch (err) {
		const message = `API Extractor failed on ${entrypoint.slice(work.length + 1)}: ${(err as Error).message}`;
		console.error(message);
		writeFileSync(join(dest, `${entry}.api.md`), `${message}\n`);
		process.exitCode = 1;
	}
}

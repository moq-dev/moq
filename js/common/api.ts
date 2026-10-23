// The API Extractor setup every `@moq/*` package shares: one report per `exports` entrypoint,
// committed beside the package and diffed by the release gate.
//
// The built `dist/` is analyzed in a copy outside the repository, where no `node_modules`
// resolves its imports, so a package's report never depends on whether its dependencies are built.

import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Extractor, ExtractorConfig, ExtractorLogLevel } from "@microsoft/api-extractor";

/**
 * Compare the built package in `dir` with its committed reports in `api/`, exiting on a
 * difference, or rewrite them when `MOQ_API_REPORT=update`, as `just fix` sets.
 */
export function checkReports(dir: string) {
	const folder = join(dir, "api");
	const current = reports(join(dir, "dist"));
	const committed = existsSync(folder) ? readdirSync(folder).filter((file) => file.endsWith(".api.md")) : [];
	const stale = [...new Set([...current.keys(), ...committed])].filter((file) => {
		const path = join(folder, file);
		return current.get(file) !== (existsSync(path) ? readFileSync(path, "utf8") : undefined);
	});

	if (process.env.MOQ_API_REPORT === "update") {
		mkdirSync(folder, { recursive: true });
		for (const file of stale) {
			const report = current.get(file);
			if (report === undefined) rmSync(join(folder, file));
			else writeFileSync(join(folder, file), report);
		}
	} else if (stale.length > 0) {
		console.error("The public API no longer matches its committed report; run `just fix` and review the diff:");
		for (const file of stale) console.error(`  api/${file}`);
		process.exit(1);
	}
}

/** Every entrypoint's API report for the built package in `dist`, by file name such as `index.api.md`. */
function reports(dist: string): Map<string, string> {
	const work = mkdtempSync(join(tmpdir(), "moq-api-"));
	try {
		const root = join(work, "package");
		cpSync(dist, root, { recursive: true });
		rewriteExternalNamespaces(root);
		const folder = join(work, "reports");
		mkdirSync(folder);

		const found = new Map<string, string>();
		const exports: Record<string, unknown> =
			JSON.parse(readFileSync(join(root, "package.json"), "utf8")).exports ?? {};
		for (const [subpath, target] of Object.entries(exports)) {
			const types = (target as { types?: string }).types;
			if (!types) continue;
			const entry = subpath === "." ? "index" : subpath.replace(/^\.\//, "").replaceAll("/", "-");
			found.set(`${entry}.api.md`, extract(root, join(root, types), folder, entry));
		}
		return found;
	} finally {
		rmSync(work, { recursive: true, force: true });
	}
}

// API Extractor cannot follow `export * as X from "<package>"`, which every entrypoint uses to
// re-export its dependencies, but reads the equivalent import-then-export form.
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

function extract(root: string, entrypoint: string, folder: string, entry: string): string {
	const config = ExtractorConfig.prepare({
		configObject: {
			projectFolder: root,
			newlineKind: "lf",
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
				reportFolder: folder,
				reportTempFolder: join(root, ".api-extractor"),
				reportFileName: entry,
			},
			docModel: { enabled: false },
			dtsRollup: { enabled: false },
			tsdocMetadata: { enabled: false },
			// The report is the output. Its findings about doc comments and unresolved
			// dependencies are left to the build's own declaration checks.
			messages: {
				compilerMessageReporting: { default: { logLevel: ExtractorLogLevel.None } },
				extractorMessageReporting: { default: { logLevel: ExtractorLogLevel.None } },
				tsdocMessageReporting: { default: { logLevel: ExtractorLogLevel.None } },
			},
		},
		configObjectFullPath: join(root, "api-extractor.json"),
		packageJsonFullPath: join(root, "package.json"),
	});

	// localBuild writes the report into the scratch folder instead of failing because it is new,
	// and the console chatter about that is suppressed; errors still fail the result.
	const result = Extractor.invoke(config, {
		localBuild: true,
		messageCallback: (message) => {
			if (message.logLevel !== ExtractorLogLevel.Error) message.handled = true;
		},
	});
	if (!result.succeeded) throw new Error(`API Extractor failed on ${entry}: ${result.errorCount} errors`);

	// Keep only the API: whether an item has a doc comment is not part of it, and every exported
	// item is @public, so neither marker churns the report or reads as a break.
	return readFileSync(join(folder, `${entry}.api.md`), "utf8")
		.replaceAll(" (undocumented)", "")
		.replace(/^ *\/\/( @public| \(No @packageDocumentation comment for this package\))?\n/gm, "");
}

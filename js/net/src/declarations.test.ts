import { expect, test } from "bun:test";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";

const pkg = resolve(import.meta.dir, "..");
const tsc = join(dirname(Bun.resolveSync("typescript/package.json", pkg)), "lib/tsc.js");

// `stripInternal` drops the export from the target `.d.ts` but leaves the import in any file
// that names the type, so a published consumer sees a module that does not export it.
test("emitted declarations import only names the target file exports", () => {
	const outDir = mkdtempSync(join(tmpdir(), "moq-net-dts-"));
	try {
		const result = Bun.spawnSync(
			["bun", tsc, "-p", "tsconfig.build.json", "--emitDeclarationOnly", "--outDir", outDir],
			{ cwd: pkg, stdout: "pipe", stderr: "pipe" },
		);
		expect(result.exitCode, result.stderr.toString() || result.stdout.toString()).toBe(0);

		const files = dtsFiles(outDir);
		expect(files.length).toBeGreaterThan(0);

		const parsed = new Map<string, FileExports>();
		for (const file of files) {
			parsed.set(file, fileExports(readFileSync(file, "utf8")));
		}

		const problems: string[] = [];
		for (const [file, info] of parsed) {
			for (const { specifier, names } of info.imports) {
				const target = dtsPath(file, specifier);
				if (!target) continue;

				if (!parsed.has(target)) {
					problems.push(`${relative(outDir, file)}: imports ${specifier}, which emitted no declarations`);
					continue;
				}

				for (const name of names) {
					if (!exported(parsed, target, name)) {
						problems.push(
							`${relative(outDir, file)}: imports ${name} from ${specifier}, which does not export it`,
						);
					}
				}
			}
		}

		expect(problems).toEqual([]);
	} finally {
		rmSync(outDir, { recursive: true, force: true });
	}
});

type FileExports = {
	names: Set<string>;
	stars: string[];
	imports: Array<{ specifier: string; names: string[] }>;
};

function dtsFiles(dir: string): string[] {
	const found: string[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) found.push(...dtsFiles(path));
		else if (entry.name.endsWith(".d.ts")) found.push(path);
	}
	return found;
}

function dtsPath(from: string, specifier: string): string | undefined {
	if (!specifier.startsWith(".")) return undefined;
	return `${resolve(dirname(from), specifier).replace(/\.(js|ts)$/, "")}.d.ts`;
}

function exported(files: Map<string, FileExports>, file: string, name: string, seen = new Set<string>()): boolean {
	if (seen.has(file)) return false;
	seen.add(file);

	const info = files.get(file);
	if (!info) return false;
	if (info.names.has(name)) return true;

	for (const specifier of info.stars) {
		const target = dtsPath(file, specifier);
		if (target && exported(files, target, name, seen)) return true;
	}
	return false;
}

function fileExports(source: string): FileExports {
	const names = new Set<string>();
	const stars: string[] = [];
	const imports: Array<{ specifier: string; names: string[] }> = [];

	const named = /^(import|export)(?:\s+type)?\s*\{([\s\S]*?)\}\s*(?:from\s+["']([^"']+)["'])?/gm;
	const star = /^export\s+\*\s+from\s+["']([^"']+)["']/gm;
	const asStar = /^export\s+\*\s+as\s+([A-Za-z_$][\w$]*)\s+from\s+["']([^"']+)["']/gm;
	const decl =
		/^export\s+(?:declare\s+)?(?:abstract\s+)?(?:type|interface|class|function|const|enum|namespace)\s+([A-Za-z_$][\w$]*)/gm;

	for (const match of source.matchAll(named)) {
		const [, kind, list, specifier] = match;
		if (specifier) imports.push({ specifier, names: ident(list ?? "", "imported") });
		if (kind === "export") {
			for (const name of ident(list ?? "", "exported")) names.add(name);
		}
	}

	for (const match of source.matchAll(star)) {
		if (match[1]) stars.push(match[1]);
	}

	for (const match of source.matchAll(asStar)) {
		if (match[1]) names.add(match[1]);
	}

	for (const match of source.matchAll(decl)) {
		if (match[1]) names.add(match[1]);
	}

	return { names, stars, imports };
}

function ident(list: string, side: "imported" | "exported"): string[] {
	return list
		.split(",")
		.map((part) => part.trim())
		.filter(Boolean)
		.map((part) => {
			const [left, right] = part.replace(/^type\s+/, "").split(/\s+as\s+/);
			const name = side === "imported" ? left : (right ?? left);
			return name?.trim() ?? "";
		})
		.filter(Boolean);
}

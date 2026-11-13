import { createHash } from "node:crypto";
import { readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import ts from "typescript";
import type { Reference } from "./index";

/**
 * Hash every file `generate` reads for `reference` plus each `extra` file or directory, such as
 * the generator and the lockfile, by path relative to `root`, so a checkout anywhere gets the same key.
 */
export function fingerprint({ root, packages }: Pick<Reference, "root" | "packages">, extra: string[]): string {
	root = realpathSync(root);
	const paths = new Set<string>();

	// Build each package's program as TypeDoc does, recording every file TypeScript reads, such as
	// sources, extended tsconfigs, and the manifests module resolution consults.
	const host = ts.createCompilerHost({});
	const parsed = new Map<string, ts.SourceFile | undefined>();
	const { readFile, getSourceFile } = host;
	host.readFile = (path) => {
		paths.add(path);
		return readFile(path);
	};
	// Packages share most of their files, so each is parsed once.
	host.getSourceFile = (path, ...rest) => {
		paths.add(path);
		if (!parsed.has(path)) parsed.set(path, getSourceFile(path, ...rest));
		return parsed.get(path);
	};
	const configHost: ts.ParseConfigFileHost = {
		...ts.sys,
		readFile: host.readFile,
		onUnRecoverableConfigFileDiagnostic: (diagnostic) => {
			throw new Error(ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"));
		},
	};
	for (const dir of packages) {
		// TypeDoc reads each package's manifest and its own option files, which TypeScript never opens.
		for (const name of readdirSync(dir)) {
			if (/^(package\.json|tsdoc\.json|typedoc\..+)$/.test(name)) paths.add(join(dir, name));
		}
		const config = ts.getParsedCommandLineOfConfigFile(join(dir, "tsconfig.json"), undefined, configHost);
		if (!config) throw new Error(`${dir}/tsconfig.json: unreadable`);
		ts.createProgram({ rootNames: config.fileNames, options: config.options, host });
	}
	for (const path of extra) walk(path, paths);

	const files = new Map<string, string>();
	for (const path of paths) {
		const real = resolved(path);
		const name = relative(root, real);
		// The lockfile covers installed dependencies; lookups above the checkout, such as for a
		// parent's package.json, differ between checkouts and change nothing they render.
		if (name.startsWith(`..${sep}`) || name.split(sep).includes("node_modules")) continue;
		files.set(name, real);
	}
	const hash = createHash("sha256");
	for (const name of [...files.keys()].sort()) {
		const path = files.get(name) as string;
		const content = statSync(path, { throwIfNoEntry: false })?.isFile() ? readFileSync(path) : undefined;
		hash.update(`${name}\0${content ? createHash("sha256").update(content).digest("hex") : "absent"}\0`);
	}
	return hash.digest("hex");
}

// The real path, so a package reached through a workspace link counts as itself; a missing path stays as given.
function resolved(path: string): string {
	try {
		return realpathSync(path);
	} catch {
		return resolve(path);
	}
}

function walk(path: string, into: Set<string>) {
	if (!statSync(path).isDirectory()) {
		into.add(path);
		return;
	}
	for (const entry of readdirSync(path)) {
		if (entry !== "node_modules") walk(join(path, entry), into);
	}
}

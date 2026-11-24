// Renders the JS API reference from the TSDoc in every published `@moq/*` package's source:
// one TypeDoc run over all of them, written as VitePress markdown with a sidebar.
import {
	cpSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	realpathSync,
	renameSync,
	rmSync,
	statSync,
	utimesSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Application, type Reflection, type ReflectionGroup, ReflectionKind, type TypeDocOptions } from "typedoc";
import { MarkdownPageEvent, type PluginOptions } from "typedoc-plugin-markdown";
import { dedupe } from "./dedupe";
import { fingerprint } from "./fingerprint";
import { EntrypointRouter } from "./router";
import { finish, Theme } from "./theme";

/** What to document and where. */
export interface Reference {
	/** Directory each path below is relative to in the output, such as the repository root. */
	root: string;
	/** Package directories, each documented through its `exports`. */
	packages: string[];
	/** Output directory, replaced on every run. */
	out: string;
	/** The VitePress source directory, which sidebar links are rooted at. */
	docs: string;
}

/** Render the reference, throwing on any TypeDoc warning, such as an unresolved link or an unexported type an export names. */
export async function generate({ root, packages, out, docs }: Reference) {
	// TypeScript reports sources by real path, so compare against real paths, such as macOS's `/var` -> `/private/var`.
	root = realpathSync(root);
	packages = packages.map((dir) => realpathSync(dir));
	const plugin = (name: string) => fileURLToPath(import.meta.resolve(name));
	// typedoc-vitepress-theme declares `docsRoot` without typing it.
	const options: TypeDocOptions & PluginOptions & { docsRoot: string } = {
		entryPointStrategy: "packages",
		entryPoints: packages,
		// Each package converts with these alone.
		packageOptions: {
			// `@internal` is how the workspace hides a symbol (`stripInternal`).
			excludeInternal: true,
			readme: "none",
			displayBasePath: root,
			// The site is built from main, so its source links point there without asking git.
			disableGit: true,
			sourceLinkTemplate: "https://github.com/moq-dev/moq/blob/main/{path}#L{line}",
		},
		name: "JavaScript API",
		readme: "none",
		plugin: [
			plugin("typedoc-plugin-markdown"),
			plugin("typedoc-vitepress-theme"),
			// Platform types such as `HTMLElement` link to MDN.
			plugin("typedoc-plugin-mdn-links"),
		],
		router: "entrypoint",
		theme: "moq",
		out,
		// Inherited lib.dom comments carry raw HTML that Vue refuses to compile.
		sanitizeComments: true,
		useCustomAnchors: true,
		// A parameter or type declaration member as a heading takes VitePress's own anchor, which
		// can collide with a symbol's, such as a `sync` property and the `Sync` class.
		parametersFormat: "table",
		typeDeclarationFormat: "table",
		hideBreadcrumbs: true,
		logLevel: "Warn",
		docsRoot: docs,
	};
	const app = await Application.bootstrapWithPlugins(options);
	app.renderer.defineRouter("entrypoint", EntrypointRouter);
	app.renderer.defineTheme("moq", Theme);
	app.renderer.on(MarkdownPageEvent.END, finish);

	const dirs = new Map<string, string>();
	for (const dir of packages) dirs.set(manifest(dir).name, relative(root, dir));
	app.on(Application.EVENT_PROJECT_REVIVE, (project) => {
		// Packages mode names a package's `.` entrypoint "" and the rest by subpath; name each by its import specifier.
		for (const pkg of project.children ?? []) {
			for (const entry of pkg.children ?? []) {
				if (entry.kindOf(ReflectionKind.Module))
					entry.name = entry.name ? `${pkg.name}/${entry.name}` : pkg.name;
			}
		}

		// A custom element inherits hundreds of members from `HTMLElement`, which MDN documents.
		const removed = new Set<Reflection>();
		for (const reflection of Object.values(project.reflections)) {
			const external =
				reflection.isDeclaration() && reflection.sources?.every((s) => s.fileName.includes("node_modules/"));
			if (reflection.isDeclaration() && reflection.inheritedFrom && external) {
				project.removeReflection(reflection);
				removed.add(reflection);
			}
		}
		// Removal leaves the groups built before the merge listing them.
		for (const reflection of Object.values(project.reflections)) {
			if (!reflection.isDeclaration() && !reflection.isProject()) continue;
			const keep = (children: ReflectionGroup["children"]) => children.filter((child) => !removed.has(child));
			reflection.groups = reflection.groups?.filter((group) => {
				group.children = keep(group.children);
				for (const category of group.categories ?? []) category.children = keep(category.children);
				return group.children.length > 0;
			});
		}
	});
	dedupe(app, dirs);

	// The CLI's `treatWarningsAsErrors`: a warning from conversion through rendering fails the run.
	const check = () => {
		if (app.logger.hasErrors() || app.logger.hasWarnings()) {
			throw new Error(`TypeDoc: ${app.logger.errorCount} errors and ${app.logger.warningCount} warnings`);
		}
	};
	const project = await app.convert();
	if (!project) throw new Error("TypeDoc: conversion failed");
	app.validate(project);
	check();
	await app.generateOutputs(project);
	check();
}

interface Manifest {
	name: string;
	workspaces?: string[];
	scripts?: Record<string, string>;
	devDependencies?: Record<string, string>;
}

function manifest(dir: string): Manifest {
	return JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
}

if (import.meta.main) {
	const here = dirname(fileURLToPath(import.meta.url));
	const root = resolve(here, "../../..");
	// Every published package, which this workspace must declare so changing one checks the site.
	const published = new Map<string, string>();
	for (const dir of manifest(root).workspaces ?? []) {
		const pkg = manifest(join(root, dir));
		if (pkg.name.startsWith("@moq/") && pkg.scripts?.release) published.set(pkg.name, join(root, dir));
	}
	const declared = Object.keys(manifest(here).devDependencies ?? {}).filter((name) => name.startsWith("@moq/"));
	const missing = [...published.keys()].filter((name) => !declared.includes(name));
	const extra = declared.filter((name) => !published.has(name));
	if (missing.length || extra.length) {
		throw new Error(
			`${here}/package.json must declare exactly the published packages: missing [${missing}], extra [${extra}]`,
		);
	}
	const out = join(here, "../../lib/js/api");
	const reference = { root, packages: [...published.values()], out, docs: join(here, "../..") };
	// Shared by every checkout on the machine, so a fresh worktree reuses what another rendered.
	const cache = join(process.env.XDG_CACHE_HOME || join(homedir(), ".cache"), "moq/api-reference");
	const entry = join(cache, fingerprint(reference, [here, join(root, "bun.lock"), join(root, "patches")]));
	if (existsSync(entry)) {
		// The modification time orders entries by last use for pruning.
		const now = new Date();
		utimesSync(entry, now, now);
		rmSync(out, { recursive: true, force: true });
		cpSync(entry, out, { recursive: true });
		console.log(`API reference: inputs unchanged, reusing ${entry}`);
	} else {
		const started = performance.now();
		await generate(reference);
		console.log(
			`API reference: ${published.size} packages in ${((performance.now() - started) / 1000).toFixed(1)}s`,
		);
		// Staged then renamed, so a concurrent reader never sees a partial entry.
		mkdirSync(cache, { recursive: true });
		const staging = mkdtempSync(join(cache, ".staging-"));
		cpSync(out, staging, { recursive: true });
		try {
			renameSync(staging, entry);
		} catch (error) {
			// Another checkout stored the same inputs first.
			if (!existsSync(entry)) throw error;
			rmSync(staging, { recursive: true });
		}
		// Each entry is about 1.3 MB; keep enough for every worktree a busy machine has open,
		// tolerating another checkout pruning the same entries.
		const entries = readdirSync(cache)
			.map((name) => ({ path: join(cache, name), used: statSync(join(cache, name), { throwIfNoEntry: false }) }))
			.sort((a, b) => (b.used?.mtimeMs ?? 0) - (a.used?.mtimeMs ?? 0));
		for (const { path } of entries.slice(32)) rmSync(path, { recursive: true, force: true });
	}
}

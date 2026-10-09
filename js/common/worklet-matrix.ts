// Manual check that `?worklet` output survives consumer bundlers: `just js worklet-matrix`.
//
// Builds a library around the real render worklet, capture worklet, and capture worker, installs it
// into an app, bundles the app with each bundler, and loads every script in Chromium. Default mode
// uses blob: URLs with no CSP, and hosted mode serves the copied assets under a strict CSP. A control
// run of blob: URLs under that CSP must fail, proving the CSP applies. Not in CI.
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium } from "playwright";
import { build } from "vite";
import { worklet } from "./vite-plugin-worklet";

const root = mkdtempSync(join(tmpdir(), "worklet-matrix-"));
const js = resolve(import.meta.dir, "..");
const lib = join(root, "node_modules/worklet-lib");

const run = (cmd: string[]) => {
	const result = Bun.spawnSync(cmd, { cwd: root, stdout: "pipe", stderr: "pipe" });
	if (result.exitCode !== 0) throw new Error(`${cmd.join(" ")}: ${result.stderr.toString()}`);
};

const page = (script: string, module: boolean) =>
	`<!doctype html><script${module ? ' type="module"' : ""} src="${script}"></script>`;

try {
	// Before the library lands in node_modules, which an install would prune.
	writeFileSync(join(root, "package.json"), JSON.stringify({ private: true, type: "module" }));
	run(["bun", "add", "webpack", "webpack-cli"]);

	writeFileSync(
		join(root, "lib.js"),
		[
			`export { default as render } from "${js}/watch/src/audio/render-worklet.ts?worklet";`,
			`export { default as capture } from "${js}/publish/src/audio/capture-worklet.ts?worklet";`,
			`export { default as worker } from "${js}/publish/src/video/capture-worker.ts?worklet";`,
		].join("\n"),
	);
	await build({
		configFile: false,
		root,
		logLevel: "silent",
		plugins: [worklet()],
		build: { outDir: lib, lib: { entry: join(root, "lib.js"), formats: ["es"], fileName: "index" } },
	});
	writeFileSync(
		join(lib, "package.json"),
		JSON.stringify({ name: "worklet-lib", type: "module", exports: "./index.js" }),
	);

	writeFileSync(
		join(root, "main.js"),
		`import { render, capture, worker } from "worklet-lib";
const base = location.search.includes("hosted") ? new URL("/moq/", location.href) : undefined;
window.result = (async () => {
	const context = new AudioContext();
	try {
		await context.audioWorklet.addModule(await render(base));
		await context.audioWorklet.addModule(await capture(base));
		new AudioWorkletNode(context, "render");
		new AudioWorkletNode(context, "capture", { numberOfOutputs: 0 });
		const spawned = new Worker(await worker(base));
		const ready = await new Promise((resolve, reject) => {
			spawned.onmessage = (event) => resolve(event.data.type);
			spawned.onerror = () => reject(new Error("capture worker failed to load"));
		});
		spawned.terminate();
		return ready === "ready" ? "ok" : ready;
	} catch (error) {
		return String(error);
	} finally {
		await context.close();
	}
})();
`,
	);
	writeFileSync(join(root, "index.html"), page("./main.js", true));

	const bundlers: Record<string, () => Promise<void> | void> = {
		vite: () =>
			build({ configFile: false, root, logLevel: "silent", build: { outDir: "out/vite" } }).then(() => {}),
		webpack: () => {
			run(["bunx", "webpack", "--mode", "production", "--entry", "./main.js", "-o", "out/webpack"]);
			writeFileSync(join(root, "out/webpack/index.html"), page("./main.js", false));
		},
		"esbuild iife": () => {
			run([
				`${js}/../node_modules/.bin/esbuild`,
				"main.js",
				"--bundle",
				"--format=iife",
				"--outdir=out/esbuild iife",
			]);
			writeFileSync(join(root, "out/esbuild iife/index.html"), page("./main.js", false));
		},
		bun: () => {
			run(["bun", "build", "main.js", "--outdir", "out/bun", "--target", "browser"]);
			writeFileSync(join(root, "out/bun/index.html"), page("./main.js", true));
		},
	};

	const browser = await chromium.launch();
	let failed = false;
	try {
		for (const [name, bundle] of Object.entries(bundlers)) {
			await bundle();
			const out = join(root, "out", name);
			mkdirSync(join(out, "moq"));
			cpSync(join(lib, "assets"), join(out, "moq"), { recursive: true });

			const server = Bun.serve({
				port: 0,
				async fetch(request) {
					const url = new URL(request.url);
					const file = Bun.file(join(out, url.pathname === "/" ? "index.html" : url.pathname));
					if (!(await file.exists())) return new Response(null, { status: 404 });
					// Only the document's CSP governs its scripts, workers, and worklets.
					const headers = new Headers();
					if (url.searchParams.has("csp"))
						headers.set("Content-Security-Policy", "script-src 'self'; worker-src 'self'");
					return new Response(file, { headers });
				},
			});
			try {
				for (const [mode, query, expected] of [
					["default", "", true],
					["hosted", "?hosted&csp", true],
					["control", "?csp", false],
				] as const) {
					const tab = await browser.newPage();
					await tab.goto(`http://localhost:${server.port}/${query}`);
					const result = await tab
						.waitForFunction(() => (window as unknown as { result?: Promise<string> }).result, null, {
							timeout: 10_000,
						})
						.then((handle) => handle.jsonValue())
						.catch((error) => String(error));
					await tab.close();
					failed ||= (result === "ok") !== expected;
					console.log(`${name.padEnd(13)} ${mode.padEnd(8)} ${result}`);
				}
			} finally {
				server.stop(true);
			}
		}
	} finally {
		await browser.close();
	}
	if (failed) process.exitCode = 1;
} finally {
	rmSync(root, { recursive: true, force: true });
}

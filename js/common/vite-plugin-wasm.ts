import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";

// js/common -> repo root
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const dist = join(repoRoot, "js/wasm/dist/moq.js");

// Rebuild when the Rust sources behind the bindings change.
const watchDirs = ["rs/moq-wasm/src", "rs/moq-net/src"].map((d) => join(repoRoot, d)).filter(existsSync);

function newestMtime(dir: string): number {
	let max = 0;
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const p = join(dir, entry.name);
		max = Math.max(max, entry.isDirectory() ? newestMtime(p) : statSync(p).mtimeMs);
	}
	return max;
}

function build(): void {
	// `just wasm` = cargo build (wasm32) + wasm-bindgen into js/wasm/dist. Needs the
	// nix dev shell on PATH, which `just dev` already provides.
	execFileSync("just", ["wasm"], { cwd: repoRoot, stdio: "inherit" });
}

function buildIfStale(): void {
	const distTime = existsSync(dist) ? statSync(dist).mtimeMs : 0;
	const srcTime = watchDirs.length ? Math.max(...watchDirs.map(newestMtime)) : 0;
	if (srcTime > distTime) build();
}

/**
 * Builds `@moq/wasm` (the wasm-bindgen output in `js/wasm/dist`) on demand, so a
 * consumer never has to run `just wasm` first. Rebuilds and full-reloads when the
 * `rs/moq-wasm` / `rs/moq-net` sources change.
 */
export function moqWasm(): Plugin {
	return {
		name: "moq-wasm",
		enforce: "pre",
		buildStart() {
			buildIfStale();
		},
		configureServer(server) {
			for (const d of watchDirs) server.watcher.add(d);
			server.watcher.on("change", (file) => {
				if (!watchDirs.some((d) => file.startsWith(d))) return;
				try {
					build();
					server.ws.send({ type: "full-reload" });
				} catch (err) {
					server.config.logger.error(`moq-wasm rebuild failed: ${String(err)}`);
				}
			});
		},
	};
}

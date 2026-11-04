import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { connect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { EmittedAsset, EmittedFile, PluginContext } from "rolldown";
import { createServer, type MinimalPluginContextWithoutEnvironment, type ResolvedConfig } from "vite";
import { worklet } from "./vite-plugin-worklet";

// A library build must never reference its worklets through import.meta.url: esbuild, Rollup, and Bun
// consumers drop that asset (a 404), and an esbuild IIFE throws at load. The blob stays the default,
// loaded lazily, with the hostable file emitted beside it.
//
// A bundler child process would outlive `vite build` and be reaped by init, so its CPU escapes the
// build's accounting and it lingers after the build.
// Check the plugin's modules with its real compiler, without Vite's build setup. The full library
// build and its child-process check run in the nightly test/worklet/matrix.ts on the real worklets.
test("worklet modules load a lazy blob or a hosted script, leaving no child process", async () => {
	const dir = mkdtempSync(join(tmpdir(), "worklet-"));
	try {
		const path = join(dir, "render.ts");
		writeFileSync(path, `registerProcessor("render", class extends AudioWorkletProcessor {});\n`);
		const plugin = worklet();
		if (typeof plugin.configResolved !== "function" || typeof plugin.load !== "function")
			throw new Error("worklet hooks must be functions");
		await plugin.configResolved.call(
			{} as MinimalPluginContextWithoutEnvironment,
			{ command: "build" } as ResolvedConfig,
		);
		const assets: EmittedAsset[] = [];
		const context = {
			emitFile(file: EmittedFile) {
				if (file.type !== "asset") throw new Error("worklet must emit an asset");
				assets.push(file);
				return "hosted";
			},
			addWatchFile() {},
		} as PluginContext;
		const entry = await plugin.load.call(context, `${path}?worklet`);
		const blob = await plugin.load.call(context, `${path}?worklet-blob`);

		expect(assets).toHaveLength(1);
		expect(assets[0].fileName).toBe("assets/render.js");
		expect(assets[0].source).toContain("registerProcessor");
		expect(entry).not.toContain("import.meta.url");
		expect(blob).not.toContain("import.meta.url");
		expect(entry).not.toContain("createObjectURL");
		expect(blob).toContain("URL.createObjectURL");
		expect(blob).toContain(JSON.stringify(assets[0].source));
		expect(entry).toContain(`import(${JSON.stringify(`${path}?worklet-blob`)})`);
		expect(entry).toContain(`new URL("render.js", base)`);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}

	const children = spawnSync("pgrep", ["-l", "-P", String(process.pid)], { encoding: "utf8" });
	expect(children.stdout).toBe("");
	expect(children.status).toBe(1);
});

// A page that calls assets() in dev fetches the same path a build emits, so the dev server serves it.
test("the dev server serves hosted worklets where builds emit them", async () => {
	const dir = mkdtempSync(join(tmpdir(), "worklet-"));
	const server = await createServer({
		configFile: false,
		logLevel: "silent",
		root: dir,
		base: "/app/",
		plugins: [worklet()],
		server: { port: 0, ws: false },
	});
	try {
		// The space makes the browser request a percent-encoded path.
		writeFileSync(
			join(dir, "render worklet.ts"),
			`registerProcessor("render", class extends AudioWorkletProcessor {});\n`,
		);
		writeFileSync(join(dir, "index.ts"), `export { default } from "./render worklet.ts?worklet";\n`);
		await server.listen();
		const origin = server.resolvedUrls?.local[0];
		if (!origin) throw new Error("dev server has no local URL");
		const url = new URL("assets/render worklet.js", origin);

		// Unknown until a page imports the worklet.
		expect((await fetch(url)).status).toBe(404);
		await server.transformRequest("/index.ts");
		await server.transformRequest("/render worklet.ts?worklet");

		// A target URL can't parse gets an error response instead of rejecting the async middleware.
		const status = await new Promise<string>((resolve, reject) => {
			const socket = connect(Number(url.port), url.hostname, () => {
				socket.end("GET //[ HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
			});
			let reply = "";
			socket.on("data", (chunk) => {
				reply += chunk;
			});
			socket.on("end", () => resolve(reply.split("\r\n")[0]));
			socket.on("error", reject);
		});
		expect(status).toBe("HTTP/1.1 500 Internal Server Error");

		const res = await fetch(url);
		expect(res.headers.get("content-type")).toBe("text/javascript");
		expect(await res.text()).toContain(`registerProcessor("render"`);
	} finally {
		await server.close();
		rmSync(dir, { recursive: true, force: true });
	}
});

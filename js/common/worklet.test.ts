import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { connect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build, createServer } from "vite";
import { worklet } from "./vite-plugin-worklet";

// A library build must never reference its worklets through import.meta.url: esbuild, Rollup, and Bun
// consumers drop that asset (a 404), and an esbuild IIFE throws at load. The blob stays the default,
// loaded lazily, with the hostable file emitted beside it.
//
// A bundler child process would outlive `vite build` and be reaped by init, so its CPU escapes the
// build's accounting and it lingers after the build.
//
// One build of a one-line worklet: the plugin's output doesn't depend on what the worklet imports, and
// bundling a real package graph costs over a CPU-second. The package builds bundle the real worklets.
test("library builds load worklets from a lazy blob or a hosted base, leaving no child process", async () => {
	const dir = mkdtempSync(join(tmpdir(), "worklet-"));
	const outDir = join(dir, "dist");
	try {
		writeFileSync(join(dir, "render.ts"), `registerProcessor("render", class extends AudioWorkletProcessor {});\n`);
		writeFileSync(join(dir, "index.ts"), `export { default } from "./render.ts?worklet";\n`);

		await build({
			configFile: false,
			logLevel: "silent",
			plugins: [worklet()],
			build: { outDir, minify: false, lib: { entry: join(dir, "index.ts"), formats: ["es"] } },
		});

		expect(readdirSync(join(outDir, "assets"))).toEqual(["render.js"]);

		const chunks = readdirSync(outDir).filter((name) => name.endsWith(".js"));
		const code = new Map(chunks.map((name) => [name, readFileSync(join(outDir, name), "utf8")]));
		for (const text of code.values()) expect(text).not.toContain("import.meta.url");

		const blob = chunks.filter((name) => code.get(name)?.includes("createObjectURL"));
		expect(blob).toHaveLength(1);
		const entry = chunks
			.filter((name) => !blob.includes(name))
			.map((name) => code.get(name))
			.join("\n");
		expect(entry).toContain(`import("./${blob[0]}")`);
		expect(entry).toContain(`new URL("render.js", `);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}

	expect(spawnSync("pgrep", ["-l", "-P", String(process.pid)], { encoding: "utf8" }).stdout).toBe("");
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

import { expect, test } from "bun:test";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { build } from "vite";
import { worklet } from "./vite-plugin-worklet";

// A library build must never reference its worklets through import.meta.url: esbuild, Rollup, and Bun
// consumers drop that asset (a 404), and an esbuild IIFE throws at load. The blob stays the default,
// loaded lazily, with the hostable file emitted beside it.
test("library builds load worklets from a lazy blob or a hosted base", async () => {
	const outDir = mkdtempSync(join(tmpdir(), "worklet-"));
	try {
		await build({
			configFile: false,
			logLevel: "silent",
			plugins: [worklet()],
			build: {
				outDir,
				minify: false,
				lib: { entry: resolve(import.meta.dir, "../watch/src/audio/decoder.ts"), formats: ["es"] },
				rollupOptions: { external: (id) => id.startsWith("@moq/") },
			},
		});

		expect(readdirSync(join(outDir, "assets"))).toEqual(["render-worklet.js"]);

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
		expect(entry).toContain(`new URL("render-worklet.js", `);
	} finally {
		rmSync(outDir, { recursive: true, force: true });
	}
});

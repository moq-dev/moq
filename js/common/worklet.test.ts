import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { createServer } from "vite";
import { worklet } from "./vite-plugin-worklet";
import { workletFixture } from "./worklet-fixture";

test("built worklets stay external through a Vite consumer", async () => {
	const fixture = await workletFixture();
	try {
		const library = readFileSync(join(fixture.root, "node_modules/worklet-fixture/index.js"), "utf8");
		expect(library).not.toContain("createObjectURL");
		expect(library).toContain("new URL(");
		const assets = readdirSync(join(fixture.root, "app/assets"));
		expect(assets.filter((name) => name.includes("worklet") && name.endsWith(".js"))).toHaveLength(2);
		const entry = assets.find((name) => name.startsWith("index-"));
		if (!entry) throw new Error("consumer entry was not emitted");
		const app = readFileSync(join(fixture.root, "app/assets", entry), "utf8");
		expect(app).not.toContain("data:");
		expect(app).not.toContain("createObjectURL");
	} finally {
		fixture.close();
	}
});

test("development worklets remain blob URLs", async () => {
	const server = await createServer({
		configFile: false,
		logLevel: "silent",
		plugins: [worklet()],
		server: { middlewareMode: true },
	});
	try {
		for (const name of ["watch/src/audio/render-worklet.ts", "publish/src/audio/capture-worklet.ts"]) {
			const result = await server.transformRequest(`${resolve(import.meta.dir, "..", name)}?worklet`);
			expect(result?.code).toContain("URL.createObjectURL");
			expect(result?.code).not.toContain("ROLLUP_FILE_URL");
		}
	} finally {
		await server.close();
	}
});

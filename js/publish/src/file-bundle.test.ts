import { expect, test } from "bun:test";
import { resolve } from "node:path";
import { gzipSync } from "node:zlib";
import { build } from "vite";

test("publish entrypoints load mediabunny only through a dynamic chunk", async () => {
	const root = resolve(import.meta.dir, "..");
	const result = await build({
		root,
		configFile: resolve(root, "vite.config.ts"),
		logLevel: "silent",
		build: { write: false, minify: true },
	});
	const output = Array.isArray(result) ? result[0] : result;
	if (!output || "close" in output) throw new Error("Expected one ES library build");
	const chunks = new Map(
		output.output.filter((output) => output.type === "chunk").map((chunk) => [chunk.fileName, chunk]),
	);
	const media = [...chunks.values()].filter((chunk) =>
		Object.keys(chunk.modules).some((id) => id.includes("/mediabunny/")),
	);
	expect(media.length).toBeGreaterThan(0);

	for (const name of ["element", "ui/element", "index"]) {
		const entry = [...chunks.values()].find((chunk) => chunk.isEntry && chunk.name === name);
		if (!entry) throw new Error(`Missing ${name} entrypoint`);
		const eager = new Set<string>();
		const visit = (file: string) => {
			if (eager.has(file)) return;
			const chunk = chunks.get(file);
			if (!chunk) return; // Workspace dependencies remain external.
			eager.add(file);
			for (const imported of chunk.imports) visit(imported);
		};
		visit(entry.fileName);
		const code = [...eager].map((file) => chunks.get(file)?.code).join("\n");
		console.info(`${name}: eager ${Buffer.byteLength(code)} bytes, ${gzipSync(code).byteLength} gzip bytes`);
		for (const chunk of media) expect(eager.has(chunk.fileName)).toBe(false);
		// The UI only attaches controls; the publisher and public source entrypoints own decoding.
		if (name !== "ui/element") {
			expect(
				[...eager].some((file) =>
					chunks
						.get(file)
						?.dynamicImports.some((imported) => media.some((chunk) => chunk.fileName === imported)),
				),
			).toBe(true);
		}
	}
});

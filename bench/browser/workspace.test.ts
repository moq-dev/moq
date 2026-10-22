import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { workspace } from "./workspace.ts";

for (const current of [false, true]) {
	test(`CMAF benchmark resolves the ${current ? "optional" : "baseline container"} export`, async () => {
		const root = await mkdtemp(join(tmpdir(), "moq-bench-workspace-"));
		try {
			const dir = join(root, "js/hang");
			await mkdir(dir, { recursive: true });
			await writeFile(
				join(dir, "package.json"),
				JSON.stringify({
					name: "@moq/hang",
					exports: { "./container": "./container.ts", ...(current ? { "./cmaf": "./cmaf.ts" } : {}) },
				}),
			);
			await writeFile(
				join(dir, "container.ts"),
				'export const Cmaf = { decodeInitSegment: () => "legacy-init", decodeDataSegment: () => "legacy" };',
			);
			await writeFile(
				join(dir, "cmaf.ts"),
				'export const decodeInitSegment = () => "current-init"; export const decodeDataSegment = () => "current";',
			);
			const entry = join(root, "entry.ts");
			await writeFile(entry, 'export { decodeInitSegment, decodeDataSegment } from "@moq/hang/cmaf";');
			const result = await Bun.build({
				entrypoints: [entry],
				outdir: join(root, "dist"),
				plugins: [workspace(root)],
			});
			expect(result.success).toBe(true);
			const decoder = await import(result.outputs[0].path);
			expect(decoder.decodeInitSegment()).toBe(current ? "current-init" : "legacy-init");
			expect(decoder.decodeDataSegment()).toBe(current ? "current" : "legacy");
		} finally {
			await rm(root, { recursive: true, force: true });
		}
	});
}

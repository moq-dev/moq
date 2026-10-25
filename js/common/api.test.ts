import { afterEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { checkReports } from "./api.ts";

const directories: string[] = [];
const previousMode = process.env.MOQ_API_REPORT;

afterEach(() => {
	for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
	if (previousMode === undefined) delete process.env.MOQ_API_REPORT;
	else process.env.MOQ_API_REPORT = previousMode;
});

function fixture(body: string): string {
	const directory = mkdtempSync(join(tmpdir(), "moq-api-test-"));
	directories.push(directory);
	mkdirSync(join(directory, "dist"));
	writeFileSync(
		join(directory, "dist/package.json"),
		JSON.stringify({ name: "@moq/example", version: "0.0.0", exports: { ".": { types: "./index.d.ts" } } }),
	);
	writeFileSync(join(directory, "dist/index.d.ts"), `/**\n * A package summary.\n * @module\n */\n${body}`);
	process.env.MOQ_API_REPORT = "update";
	return directory;
}

test("a documented public declaration produces a report with the shared module tag", () => {
	const directory = fixture("/** Read a value.\n * @public\n */\nexport declare function read(): string;\n");
	checkReports(directory);
	expect(readFileSync(join(directory, "api/index.api.md"), "utf8")).toContain("export function read(): string;");
});

test("a public signature cannot depend on a declaration absent from its entrypoint", () => {
	const directory = fixture('export { read } from "./access";\n');
	writeFileSync(
		join(directory, "dist/access.d.ts"),
		`/** Input to a read.\n * @public\n */
export interface AccessInput {
/** The path to read. */
path: string;
}
/** Read a value.\n * @public\n */
export declare function read(input: AccessInput): string;
`,
	);
	expect(() => checkReports(directory)).toThrow("API Extractor failed");
});

test("a stripped declaration cannot remain in a public signature", () => {
	const directory = fixture(`/** Read a value.\n * @public\n */
export declare function read(input: AccessInput): string;
`);
	expect(() => checkReports(directory)).toThrow();
});

test("a public declaration needs a summary, even with a release tag", () => {
	const directory = fixture("/** @public */\nexport declare function read(): string;\n");
	expect(() => checkReports(directory)).toThrow("API Extractor failed");
});

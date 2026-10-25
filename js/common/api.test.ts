import { afterEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { checkReports } from "./api.ts";

const directories: string[] = [];
const previousMode = process.env.MOQ_API_REPORT;

afterEach(() => {
	for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
	if (previousMode === undefined) delete process.env.MOQ_API_REPORT;
	else process.env.MOQ_API_REPORT = previousMode;
});

const summary = "/**\n * A package summary.\n * @module\n */\n";

function fixture(files: Record<string, string>, manifest: Record<string, unknown> = {}): string {
	const directory = mkdtempSync(join(tmpdir(), "moq-api-test-"));
	directories.push(directory);
	const all: Record<string, string> = {
		"dist/package.json": JSON.stringify({
			name: "@moq/example",
			version: "0.0.0",
			exports: { ".": { types: "./index.d.ts" } },
			...manifest,
		}),
		...files,
	};
	for (const [path, body] of Object.entries(all)) {
		mkdirSync(dirname(join(directory, path)), { recursive: true });
		writeFileSync(join(directory, path), body);
	}
	process.env.MOQ_API_REPORT = "update";
	return directory;
}

test("a documented declaration produces a report", () => {
	const directory = fixture({
		"dist/index.d.ts": `${summary}/** Read a value. */\nexport declare function read(): string;\n`,
	});
	checkReports(directory);
	expect(readFileSync(join(directory, "api/index.api.md"), "utf8")).toContain("export function read(): string;");
});

test("a declaration needs a summary", () => {
	// The module summary would document a declaration directly after it.
	const directory = fixture({
		"dist/index.d.ts": `${summary}/** Write a value. */\nexport declare function write(): void;\nexport declare function read(): string;\n`,
	});
	expect(() => checkReports(directory)).toThrow("API Extractor failed");
});

test("a signature cannot depend on a declaration no entrypoint exports", () => {
	const directory = fixture({
		"dist/index.d.ts": `${summary}export { read } from "./access";\n`,
		"dist/access.d.ts": `/** Input to a read. */
export interface AccessInput {
	/** The path to read. */
	path: string;
}
/** Read a value. */
export declare function read(input: AccessInput): string;
`,
	});
	expect(() => checkReports(directory)).toThrow("API Extractor failed");
});

test("a signature may use a declaration another entrypoint exports, or one it renames", () => {
	const directory = fixture(
		{
			"dist/index.d.ts": `${summary}export { type AccessInput } from "./access";\n`,
			"dist/access.d.ts": `/** Input to a read. */
export interface AccessInput {
	/** The path to read. */
	path: string;
}
/** Parse an input. */
declare function parse(text: string): AccessInput;
/** Reads values. */
export declare class Reader {
	/** Read a value. */
	read(input: AccessInput): string;
	/** Parse an input. */
	static parse: typeof parse;
}
`,
			"dist/reader.d.ts": `${summary}export { Reader } from "./access";\n`,
		},
		{ exports: { ".": { types: "./index.d.ts" }, "./reader": { types: "./reader.d.ts" } } },
	);
	checkReports(directory);
	expect(readFileSync(join(directory, "api/reader.api.md"), "utf8")).toContain("static parse: typeof parse;");
});

test("a declaration resolves types from declared dependencies only", () => {
	const files = {
		"node_modules/dep/package.json": JSON.stringify({ name: "dep", version: "0.0.0", types: "./index.d.ts" }),
		"node_modules/dep/index.d.ts": "export interface Value {\n\tvalue: string;\n}\n",
		"dist/index.d.ts": `${summary}import type { Value } from "dep";\n/** Read a value. */\nexport declare function read(): Value;\n`,
	};
	checkReports(fixture(files, { dependencies: { dep: "^0.0.0" } }));
	expect(() => checkReports(fixture(files))).toThrow("API Extractor failed");
});

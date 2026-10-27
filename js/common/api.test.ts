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

/** The declarations in a report, without its heading and fence. */
function report(directory: string, file = "index.api.md"): string {
	return (
		readFileSync(join(directory, "api", file), "utf8")
			.split("```ts\n")[1]
			?.split("```")[0] ?? ""
	);
}

// A dependency named `dep`, installed beside the package and declared by it.
const dep = {
	"node_modules/dep/package.json": JSON.stringify({
		name: "dep",
		version: "0.0.0",
		exports: { ".": { types: "./index.d.ts" } },
	}),
	"node_modules/dep/index.d.ts": "/** A value. */\nexport interface Value {\n\tvalue: string;\n}\n",
};

test("a report spells each declaration as TypeScript, one member per line", async () => {
	const directory = fixture({
		"dist/index.d.ts": `/** Make a value. */
export declare const Value: ((text: string) => Value) & {
	readonly zero: Value;
};
/** A value. */
export type Value = string | (() => string)[];
/** Reads values. */
export declare class Reader<T extends Value = Value> {
	/** Read one. */
	read(options?: { signal?: AbortSignal }): Promise<T>;
	/** Start reading. */
	constructor(source: Value);
	/** How many were read. */
	get count(): number;
}
declare const _default: Reader;
export default _default;
`,
	});
	await checkReports(directory);
	expect(report(directory)).toBe(`export class Reader<T extends Value = Value> {
	constructor(source: Value);
	get count(): number;
	read(options?: {
		signal?: AbortSignal;
	}): Promise<T>;
}

export const Value: ((text: string) => Value) & {
	readonly zero: Value;
};

export type Value = string | (() => string)[];

declare const _default: Reader;
export default _default;
`);
});

test("a signature cannot depend on a declaration no entrypoint exports", async () => {
	const directory = fixture({
		"dist/index.d.ts": `export { read } from "./access";\n`,
		"dist/access.d.ts": `/** Input to a read. */
export interface AccessInput {
	/** The path to read. */
	path: string;
}
/** Read a value. */
export declare function read(input: AccessInput): string;
`,
	});
	await expect(checkReports(directory)).rejects.toThrow("AccessInput, used by read, is not exported");
});

test("a signature may use a declaration another entrypoint exports, or an alias re-exports", async () => {
	const directory = fixture(
		{
			"dist/index.d.ts": `export { type AccessInput } from "./access";\n`,
			"dist/access.d.ts": `/** Options for a read. */
interface Options {
	/** Whether to wait. */
	wait: boolean;
}
/** Options for a read. @reexport */
export type ReadOptions = Options;
/** Input to a read. */
export interface AccessInput {
	/** The path to read. */
	path: string;
}
/** Reads values. */
export declare class Reader {
	/** Read a value. */
	read(input: AccessInput, options: Options): string;
	/** Read a value without waiting. */
	peek(options: ReadOptions): string;
}
`,
			"dist/reader.d.ts": `export { Reader, type ReadOptions } from "./access";\n`,
		},
		{ exports: { ".": { types: "./index.d.ts" }, "./reader": { types: "./reader.d.ts" } } },
	);
	await checkReports(directory);
	expect(report(directory, "reader.api.md")).toBe(`import { AccessInput } from "@moq/example";

export interface ReadOptions {
	wait: boolean;
}

export class Reader {
	constructor();
	peek(options: ReadOptions): string;
	read(input: AccessInput, options: ReadOptions): string;
}
`);
});

test("another package's declarations are reported as the re-export that brings them", async () => {
	const directory = fixture(
		{
			...dep,
			"dist/index.d.ts": `export * as Dep from "dep";\nexport { type Value as Thing } from "dep";\n`,
		},
		{ dependencies: { dep: "^0.0.0" } },
	);
	await checkReports(directory);
	expect(report(directory)).toBe(`export * as Dep from "dep";

export { Value as Thing } from "dep";
`);
});

test("a reference names its declaration by where its package exports it", async () => {
	const directory = fixture(
		{
			"node_modules/dep/package.json": JSON.stringify({
				name: "dep",
				version: "0.0.0",
				exports: { ".": { types: "./index.d.ts" } },
			}),
			"node_modules/dep/index.d.ts": `export * as Audio from "./audio";\nexport * as Video from "./video";\n`,
			"node_modules/dep/audio.d.ts": "export interface Decoder {\n\trate: number;\n}\n",
			"node_modules/dep/video.d.ts": "export interface Decoder {\n\tframe: number;\n}\n",
			"dist/index.d.ts": `import type { Audio as A, Video } from "dep";
/** An audio track. */
export interface Audio {
	/** Decodes it. */
	decoder: A.Decoder;
}
/** A member. */
export interface Member {
	/** Its audio. */
	audio: A.Decoder;
	/** Its video. */
	video: Video.Decoder;
}
`,
		},
		{ dependencies: { dep: "^0.0.0" } },
	);
	await checkReports(directory);
	// \`Audio\` is declared here too, so the other one cannot be imported by that name.
	expect(report(directory)).toBe(`import { Video } from "dep";

export interface Audio {
	decoder: import("dep").Audio.Decoder;
}

export interface Member {
	audio: import("dep").Audio.Decoder;
	video: Video.Decoder;
}
`);
});

test("a type is reported as written, not as the checker restates it", async () => {
	const directory = fixture({
		"dist/index.d.ts": `/** Reads every value. */
export declare function all<S extends readonly unknown[]>(values: [...S]): { [K in keyof S]: S[K] | undefined };
/** A value and its fallback. */
export interface Pair<T> {
	/** Both, in order. */
	both: [...T[], T];
}
/** Pairs of strings, as the compiler writes an inferred type. */
export declare const strings: import("./index").Pair<string>[];
`,
	});
	await checkReports(directory);
	expect(report(directory)).toBe(`export interface Pair<T> {
	both: [...T[], T];
}

export function all<S extends readonly unknown[]>(values: [...S]): { [K in keyof S]: S[K] | undefined };

export const strings: Pair<string>[];
`);
});

test("an interface reports its own index signatures, not inherited ones", async () => {
	const directory = fixture({
		"dist/index.d.ts": `/** A nested list. */
export interface List extends Array<string | List> {}
/** Counts by name. */
export interface Counts {
	/** One count. */
	[name: string]: number;
}
`,
	});
	await checkReports(directory);
	expect(report(directory)).toBe(`export interface Counts {
	[name: string]: number;
}

export interface List extends Array<string | List> {}
`);
});

test("a deeply nested type is reported in full", async () => {
	const depth = 12;
	const type = `${"{ a: ".repeat(depth)}number${" }".repeat(depth)}`;
	const directory = fixture({ "dist/index.d.ts": `/** A nested value. */\nexport declare const nested: ${type};\n` });
	await checkReports(directory);
	expect(report(directory)).toContain(`${"\t".repeat(depth)}a: number;`);
});

test("a reference is spelled so it resolves where it is written", async () => {
	const directory = fixture({
		"dist/hop.d.ts": `/** A cost. */
export type Cost = bigint;
/** Costs. */
export declare const Cost: { zero: bigint };
`,
		"dist/rendition.d.ts": `/** A rendition. */
export declare class Rendition<T> {
	/** Its value. */
	value: T;
}
`,
		"dist/session.d.ts": `/** A session. */
export default class {
	/** Accept one. */
	static accept(cost: Cost): Session;
}
import type { Cost } from "./hop";
import type Session from "./session";
`,
		"dist/index.d.ts": `import { Rendition as Base } from "./rendition";
import Session from "./session";
export type { Cost } from "./hop";
export { Rendition } from "./rendition";
export default Session;
/** Audio. */
export declare namespace Audio {
	/** An audio rendition. */
	type Rendition = Base<string>;
	/** Audio, which is no namespace to hide this one. */
	type Audio = File[];
	/** An audio file. */
	interface File {
		/** The global one. */
		file: globalThis.File;
	}
}
`,
	});
	await checkReports(directory);
	expect(report(directory)).toBe(`export namespace Audio {
	export type Audio = Audio.File[];
	export interface File {
		file: globalThis.File;
	}
	export type Rendition = import("@moq/example").Rendition<string>;
}

export type Cost = bigint;

export class Rendition<T> {
	constructor();
	value: T;
}

export default class {
	constructor();
	static accept(cost: Cost): import("@moq/example").default;
}
`);
});

test("a declaration resolves types from declared dependencies only", async () => {
	const files = {
		...dep,
		"dist/index.d.ts": `import type { Value } from "dep";\n/** Read a value. */\nexport declare function read(): Value;\n`,
	};
	await checkReports(fixture(files, { dependencies: { dep: "^0.0.0" } }));
	await expect(checkReports(fixture(files))).rejects.toThrow("Cannot find module 'dep'");
});

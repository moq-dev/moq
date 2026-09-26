import { expect, test } from "bun:test";
import { problems } from "./declarations";

const root = "/dist";

test("an import of a stripped export is reported", () => {
	const files = new Map([
		["/dist/reload.d.ts", "export declare class Reload {}\n"],
		["/dist/index.d.ts", 'import { ReloadDelay } from "./reload.js";\nexport declare const delay: ReloadDelay;\n'],
	]);
	expect(problems(root, files)).toEqual([
		"index.d.ts: imports ReloadDelay from ./reload.js, which does not export it",
	]);
});

test("an import of a file with no declarations is reported", () => {
	const files = new Map([["/dist/index.d.ts", 'import type { Mock } from "./mock.ts";\n']]);
	expect(problems(root, files)).toEqual(["index.d.ts: imports ./mock.ts, which emitted no declarations"]);
});

test("names re-exported through a star, an alias, a default, or a directory index resolve", () => {
	const files = new Map([
		["/dist/inner.d.ts", "export interface Delay {}\ndeclare const status = 1;\nexport { status as Status };\n"],
		["/dist/outer/index.d.ts", 'export * from "../inner.js";\n'],
		["/dist/element.d.ts", "export default class Element {}\n"],
		["/dist/page.d.ts", 'import { Delay } from "./outer";\nimport { Status } from ".";\n'],
		[
			"/dist/index.d.ts",
			'export * from "./outer";\nexport type { default as Element } from "./element.tsx";\nimport { type Delay, Status as S } from "./outer/index.ts";\nimport { Other } from "@moq/other";\n',
		],
	]);
	expect(problems(root, files)).toEqual([]);
});

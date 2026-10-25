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
			'export * from "./outer";\nexport type { default as Element } from "./element.tsx";\nimport { type Delay, Status as S } from "./outer/index.ts";\nimport { Other } from "@moq/other";\nimport icon from "./icon.svg?raw";\n',
		],
	]);
	expect(problems(root, files)).toEqual([]);
});

test("a default import of a module without a default export is reported", () => {
	const files = new Map([
		["/dist/element.d.ts", "export declare class Element {}\n"],
		["/dist/index.d.ts", 'import type Element from "./element.js";\nexport { Element };\n'],
	]);
	expect(problems(root, files)).toEqual(["index.d.ts: imports default from ./element.js, which does not export it"]);
});

test("type-only star re-exports, const enums, and let declarations resolve", () => {
	const files = new Map([
		["/dist/types.d.ts", "export declare const enum State { Open }\nexport declare let value: number;\n"],
		["/dist/outer.d.ts", 'export type * from "./types.js";\n'],
		["/dist/index.d.ts", 'import type { State, value } from "./outer.js";\n'],
	]);
	expect(problems(root, files)).toEqual([]);
});

test("an extensionless import resolves to the file before a directory index", () => {
	const files = new Map([
		["/dist/foo.d.ts", "export declare const value: number;\n"],
		["/dist/foo/index.d.ts", "export declare const other: number;\n"],
		["/dist/index.d.ts", 'import { value } from "./foo";\n'],
	]);
	expect(problems(root, files)).toEqual([]);
});

test("a trailing slash resolves to the directory index over a file", () => {
	const files = new Map([
		["/dist/foo.d.ts", "export declare const value: number;\n"],
		["/dist/foo/index.d.ts", "export declare const other: number;\n"],
		["/dist/index.d.ts", 'import { other } from "./foo/";\n'],
	]);
	expect(problems(root, files)).toEqual([]);
});

test("a name its source declares but the declarations strip is reported", () => {
	const source = {
		path: "/src/access.ts",
		text: [
			'import { Patterns } from "@moq/pattern";',
			"/** @internal */ type Input = string | Patterns;",
			"/** @internal */ const defaults = {};",
			"/** @internal */ class Base {}",
			"export const Schema = transform<Access, Input>();",
			"export class Access extends Base { static defaults: typeof defaults; }",
		].join("\n"),
	};
	const files = new Map([
		[
			"/dist/access.d.ts",
			'import { Patterns } from "@moq/pattern";\nexport declare const Schema: Transform<Access, Input, Patterns>;\nexport declare class Access extends Base {\n    static defaults: typeof defaults.value;\n}\n',
		],
	]);
	expect(problems(root, files, new Map([["/dist/access.d.ts", source]]))).toEqual([
		"access.d.ts: references Input, which its declarations do not declare",
		"access.d.ts: references Base, which its declarations do not declare",
		"access.d.ts: references defaults, which its declarations do not declare",
	]);
});

test("globals, type parameters, and names the declarations keep resolve", () => {
	const source = {
		path: "/src/element.tsx",
		text: [
			'import type { Track } from "./track.ts";',
			"type Key = string;",
			"function Render<T>(props: { value: T }) { return <div>{String(props.value)}</div>; }",
			"export type Map<K> = { [P in Key]: K extends Promise<infer U> ? U : Track };",
			"export { Render };",
		].join("\n"),
	};
	const files = new Map([
		[
			"/dist/element.d.ts",
			'import type { Track } from "./track.ts";\ntype Key = string;\ndeclare function Render<T>(props: { value: T }): HTMLElement;\nexport type Map<K> = { [P in Key]: K extends Promise<infer U> ? U : Track };\nexport { Render };\n',
		],
		["/dist/track.d.ts", "export interface Track {}\n"],
	]);
	expect(problems(root, files, new Map([["/dist/element.d.ts", source]]))).toEqual([]);
});

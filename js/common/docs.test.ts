import { expect, test } from "bun:test";
import { undocumented } from "./docs";

const root = "/dist";

// Each entrypoint gets the `@module` block it needs, so a test covers only what it names.
function check(files: Record<string, string>, entrypoints = ["/dist/index.d.ts"]): string[] {
	const described = Object.entries(files).map(([file, source]): [string, string] =>
		entrypoints.includes(file) ? [file, `/** Test. @module */\n\n${source}`] : [file, source],
	);
	return undocumented(root, new Map(described), entrypoints);
}

test("an entrypoint without a module block is reported", () => {
	const found = undocumented(
		root,
		new Map([["/dist/index.d.ts", "/** Documented. */\nexport declare const a: number;\n"]]),
		["/dist/index.d.ts"],
	);
	expect(found).toEqual(["index.d.ts: no @module block"]);
});

test("members inherited through a namespace import are checked", () => {
	const found = check({
		"/dist/base.d.ts": "/** Documented. */\nexport declare class Parent {\n    run(): void;\n}\n",
		"/dist/index.d.ts":
			'import * as base from "./base";\n/** Documented. */\nexport declare class Derived extends base.Parent {}\n',
	});
	expect(found).toEqual(["base.d.ts: Parent.run"]);
});

test("an undocumented export and class member are reported", () => {
	const found = check({
		"/dist/index.d.ts": [
			"export declare const value: number;",
			"/** Documented. */",
			"export declare class Thing {",
			"    #private;",
			"    /** Documented. */",
			"    readonly name: string;",
			"    close(): void;",
			"}",
		].join("\n"),
	});
	expect(found).toEqual(["index.d.ts: value", "index.d.ts: Thing.close"]);
});

test("a line comment or an empty block is not a doc comment", () => {
	const found = check({
		"/dist/index.d.ts": "// Not a doc.\nexport declare const a: number;\n/** */\nexport declare const b: number;\n",
	});
	expect(found).toEqual(["index.d.ts: a", "index.d.ts: b"]);
});

test("re-exports resolve through stars, namespaces, aliases, imports, and defaults", () => {
	const found = check({
		"/dist/inner.d.ts":
			"/** Documented. */\nexport declare function documented(): void;\nexport declare function bare(): void;\n",
		"/dist/ns/index.d.ts": "export type Hidden = number;\n",
		"/dist/element.d.ts": "/** Documented. */\nexport default class Element {\n    get url(): string;\n}\n",
		"/dist/local.d.ts": "declare const local: number;\nexport { local as renamed };\n",
		"/dist/index.d.ts": [
			'export * from "./inner.js";',
			"/** Documented. */",
			'export * as Ns from "./ns";',
			'export { default as Element } from "./element.ts";',
			'export { renamed } from "./local";',
			'import { documented } from "./inner";',
			"export { documented as again };",
			"/** Documented. */",
			'export * as External from "@moq/other";',
			'export { Other } from "@moq/other";',
		].join("\n"),
	});
	expect(found).toEqual([
		"ns/index.d.ts: Ns.Hidden",
		"element.d.ts: Element.url",
		"local.d.ts: renamed",
		"inner.d.ts: bare",
	]);
});

test("only the public surface needs docs", () => {
	const found = check({
		"/dist/internal.d.ts": "export declare const unexported: number;\n",
		"/dist/index.d.ts": [
			'import "./internal";',
			"/** Documented. */",
			"export declare class Element extends HTMLElement {",
			"    #private;",
			"    private secret;",
			"    protected hook(): void;",
			"    static observedAttributes: string[];",
			"    constructor();",
			"    connectedCallback(): void;",
			"    readonly [BRAND] = true;",
			"    [key: string]: unknown;",
			"}",
		].join("\n"),
	});
	expect(found).toEqual([]);
});

test("members inherited from an unexported package class are checked", () => {
	const found = check({
		"/dist/base.d.ts": "export declare class Base {\n    run(): void;\n}\n",
		"/dist/index.d.ts": [
			'import { Base } from "./base";',
			"/** Documented. */",
			"export declare class Thing extends Base {}",
			"/** Documented. */",
			"export declare class Failure extends Error {}",
		].join("\n"),
	});
	expect(found).toEqual(["base.d.ts: Base", "base.d.ts: Base.run"]);
});

test("members inherited from a class in a declared namespace are checked", () => {
	const found = check({
		"/dist/index.d.ts": [
			"declare namespace Hidden {",
			"    class Parent {",
			"        run(): void;",
			"    }",
			"}",
			"/** Documented. */",
			"export declare class Derived extends Hidden.Parent {}",
		].join("\n"),
	});
	expect(found).toEqual(["index.d.ts: Hidden.Parent", "index.d.ts: Hidden.Parent.run"]);
});

test("members inherited through nested namespaces are checked", () => {
	const found = check({
		"/dist/base.d.ts": "export declare namespace Inner {\n    class Parent {\n        run(): void;\n    }\n}\n",
		"/dist/index.d.ts":
			'import * as ns from "./base";\n/** Documented. */\nexport declare class Derived extends ns.Inner.Parent {}\n',
	});
	expect(found).toEqual(["base.d.ts: Inner.Parent", "base.d.ts: Inner.Parent.run"]);
});

test("a dotted namespace is checked like a nested one", () => {
	const found = check({
		"/dist/index.d.ts": [
			"/** Documented. */",
			"export declare namespace Outer.Inner {",
			"    class Parent {",
			"        run(): void;",
			"    }",
			"}",
			"/** Documented. */",
			"export declare class Derived extends Outer.Inner.Parent {}",
		].join("\n"),
	});
	expect(found).toEqual(["index.d.ts: Outer.Inner.Parent", "index.d.ts: Outer.Inner.Parent.run"]);
});

test("only a block above the first statement is the module block", () => {
	const source =
		"/** Documented. */\nexport declare const a: number;\n/** Mentions @module. */\nexport declare const b: number;\n";
	const found = undocumented(root, new Map([["/dist/index.d.ts", source]]), ["/dist/index.d.ts"]);
	expect(found).toEqual(["index.d.ts: no @module block"]);
});

test("a type-only star is checked like a value star", () => {
	const found = check({
		"/dist/inner.d.ts": "export declare const value: number;\nexport type Shape = number;\n",
		"/dist/index.d.ts": 'export type * from "./inner";\n',
	});
	expect(found).toEqual(["inner.d.ts: value", "inner.d.ts: Shape"]);
});

test("an extensionless specifier prefers the file over a directory index", () => {
	const found = check({
		"/dist/foo.d.ts": "export declare const file: number;\n",
		"/dist/foo/index.d.ts": "export declare const index: number;\n",
		"/dist/index.d.ts": 'export * from "./foo";\n',
	});
	expect(found).toEqual(["foo.d.ts: file"]);
});

test("a namespace re-export needs its own doc, whichever package it names", () => {
	const found = check({
		"/dist/ns.d.ts": "/** Documented. */\nexport declare const value: number;\n",
		"/dist/index.d.ts": 'export * as Local from "./ns";\nexport * as External from "@moq/other";\n',
	});
	expect(found).toEqual(["index.d.ts: Local", "index.d.ts: External"]);
});

test("a well-known symbol member needs a doc, unlike a brand", () => {
	const found = check({
		"/dist/index.d.ts":
			"/** Documented. */\nexport declare class Handle {\n    readonly [BRAND] = true;\n    [Symbol.dispose](): void;\n}\n",
	});
	expect(found).toEqual(["index.d.ts: Handle.[Symbol.dispose]"]);
});

test("a type-only namespace re-export is checked like a value one", () => {
	const found = check({
		"/dist/inner.d.ts": "export declare const value: number;\nexport type Shape = number;\n",
		"/dist/index.d.ts": '/** Documented. */\nexport type * as Types from "./inner";\n',
	});
	expect(found).toEqual(["inner.d.ts: Types.value", "inner.d.ts: Types.Shape"]);
});

test("an explicit export shadows the same name from a star", () => {
	const found = check({
		"/dist/inner.d.ts": "export declare const name: number;\n",
		"/dist/index.d.ts": '/** Documented. */\nexport declare const name: number;\nexport * from "./inner";\n',
	});
	expect(found).toEqual([]);
});

test("a named default declaration is also exported by its name", () => {
	const found = check({
		"/dist/index.d.ts": "/** Documented. */\nexport default class Thing {}\nexport { Thing };\n",
	});
	expect(found).toEqual([]);
});

test("a module block does not document the statement after it", () => {
	const found = check({
		"/dist/index.d.ts": "/**\n * The package.\n *\n * @module\n */\nexport declare const first: number;\n",
	});
	expect(found).toEqual(["index.d.ts: first"]);
});

test("a literal computed key is an ordinary member", () => {
	const found = check({
		"/dist/index.d.ts":
			'/** Documented. */\nexport declare class Thing {\n    ["parse"](): void;\n    [42]: string;\n}\n',
	});
	expect(found).toEqual(["index.d.ts: Thing.parse", "index.d.ts: Thing.42"]);
});

test("a star re-export skips the default export", () => {
	const found = check({
		"/dist/inner.d.ts": "export default class Hidden {}\n/** Documented. */\nexport declare const shown: number;\n",
		"/dist/index.d.ts": 'export * from "./inner";\n',
	});
	expect(found).toEqual([]);
});

test("custom element reactions are exempt only on an element", () => {
	const found = check({
		"/dist/index.d.ts": [
			"/** Documented. */",
			"export declare class Hooks {",
			"    connectedCallback(): void;",
			"}",
		].join("\n"),
	});
	expect(found).toEqual(["index.d.ts: Hooks.connectedCallback"]);
});

test("a namespace merged with a class has its members checked", () => {
	const found = check({
		"/dist/index.d.ts": [
			"/** Documented. */",
			"export declare class Producer {}",
			"export declare namespace Producer {",
			"    /** Documented. */",
			"    type Config = number;",
			"    type Bare = string;",
			"    class Inner {",
			"        run(): void;",
			"    }",
			"}",
		].join("\n"),
	});
	expect(found).toEqual([
		"index.d.ts: Producer.Bare",
		"index.d.ts: Producer.Inner",
		"index.d.ts: Producer.Inner.run",
	]);
});

test("one documented overload or accessor covers the name", () => {
	const found = check({
		"/dist/index.d.ts": [
			"/** Documented. */",
			"export declare function parse(text: string): number;",
			"export declare function parse(text: string, radix: number): number;",
			"/** Documented. */",
			"export declare class Thing {",
			"    /** Documented. */",
			"    get value(): number;",
			"    set value(v: number);",
			"}",
		].join("\n"),
	});
	expect(found).toEqual([]);
});

test("a re-export of a missing name fails loudly", () => {
	expect(() => check({ "/dist/a.d.ts": "", "/dist/index.d.ts": 'export { gone } from "./a";\n' })).toThrow(
		"index.d.ts: ./a does not export gone",
	);
});

test("an export marker in a namespace hides its unexported members", () => {
	const found = check({
		"/dist/index.d.ts": [
			"/** Documented. */",
			"export declare namespace Ns {",
			"    function helper(): void;",
			"    /** Documented. */",
			"    export function run(): void;",
			"    export function bare(): void;",
			"    export {};",
			"}",
		].join("\n"),
	});
	expect(found).toEqual(["index.d.ts: Ns.bare"]);
});

test("an interface merged into a class has its members checked", () => {
	const found = check({
		"/dist/index.d.ts": [
			"/** Documented. */",
			"export declare class Thing {}",
			"export interface Thing {",
			"    /** Documented. */",
			"    name: string;",
			"    run(): void;",
			"}",
		].join("\n"),
	});
	expect(found).toEqual(["index.d.ts: Thing.run"]);
});

test("a static member does not document the instance member of the same name", () => {
	const found = check({
		"/dist/index.d.ts": [
			"/** Documented. */",
			"export declare class Thing {",
			"    /** Documented. */",
			"    static create(): Thing;",
			"    create(): void;",
			"}",
		].join("\n"),
	});
	expect(found).toEqual(["index.d.ts: Thing.create"]);
});

test("members inherited from an unexported sibling in a namespace are checked", () => {
	const found = check({
		"/dist/index.d.ts": [
			"/** Documented. */",
			"export declare namespace Ns {",
			"    class Base {",
			"        run(): void;",
			"    }",
			"    /** Documented. */",
			"    export class Public extends Base {}",
			"    export {};",
			"}",
		].join("\n"),
	});
	expect(found).toEqual(["index.d.ts: Ns.Base", "index.d.ts: Ns.Base.run"]);
});

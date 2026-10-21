import { expect, test } from "bun:test";
import { undocumented } from "./docs";

const root = "/dist";

function check(files: Record<string, string>, entrypoints = ["/dist/index.d.ts"]): string[] {
	return undocumented(root, new Map(Object.entries(files)), entrypoints);
}

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
			'export * as Ns from "./ns";',
			'export { default as Element } from "./element.ts";',
			'export { renamed } from "./local";',
			'import { documented } from "./inner";',
			"export { documented as again };",
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

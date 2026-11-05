// Renders small packages through the real pipeline, so each case covers conversion, the
// dedupe plugin, the router, and VitePress's markdown together.
import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createMarkdownRenderer } from "vitepress";
import { generate } from "./index";

const md = await createMarkdownRenderer(join(dirname(fileURLToPath(import.meta.url)), "../.."));
const roots: string[] = [];
afterAll(() => {
	for (const root of roots) rmSync(root, { recursive: true, force: true });
});

const tsconfig = JSON.stringify({
	compilerOptions: {
		strict: true,
		target: "ES2022",
		module: "ESNext",
		moduleResolution: "Bundler",
		skipLibCheck: true,
		lib: ["ES2022", "DOM"],
	},
	include: ["src"],
});

interface Package {
	exports?: Record<string, string>;
	files: Record<string, string>;
}

/** Write each package into a fresh workspace and render it, returning the page reader. */
async function render(packages: Record<string, Package>) {
	const root = mkdtempSync(join(tmpdir(), "moq-doc-api-"));
	roots.push(root);
	const write = (path: string, text: string) => {
		mkdirSync(dirname(join(root, path)), { recursive: true });
		writeFileSync(join(root, path), text);
	};
	for (const [name, pkg] of Object.entries(packages)) {
		const exports = pkg.exports ?? { ".": "./src/index.ts" };
		write(`${name}/package.json`, JSON.stringify({ name: `@test/${name}`, version: "0.0.0", exports }));
		write(`${name}/tsconfig.json`, tsconfig);
		for (const [path, text] of Object.entries(pkg.files)) write(`${name}/${path}`, text);
		// Each package can import the others, as workspace links allow.
		for (const other of Object.keys(packages)) {
			if (other === name) continue;
			mkdirSync(join(root, name, "node_modules/@test"), { recursive: true });
			symlinkSync(join(root, other), join(root, name, "node_modules/@test", other));
		}
	}
	const out = join(root, "out");
	await generate({ root, packages: Object.keys(packages).map((name) => join(root, name)), out, docs: root });
	return (page: string) => {
		const text = readFileSync(join(out, page), "utf8");
		// VitePress refuses a page whose anchors collide, which the build would hit.
		md.render(text);
		return text;
	};
}

describe("a rendered workspace", async () => {
	const page = await render({
		owner: {
			exports: { ".": "./src/index.ts", "./sub": "./src/sub.ts" },
			files: {
				"src/index.ts": `/** A thing the owner defines. */
export class Thing {}
`,
				"src/sub.ts": `/** A value only the subpath exports. */
export const sub = 1;
`,
			},
		},
		user: {
			files: {
				"src/index.ts": `export { Thing } from "@test/owner";
export { Thing as First, Thing as Second } from "dep";
export * as Error from "./errors";
export * from "./value";
`,
				"src/errors.ts": `/** Return the builtin error. */
export function fail(): Error {
	return new Error("fail");
}
`,
				"src/value.ts": `/** The declaration. */
interface Value {
	/** Its text. */
	text: string;
}
/** The public name of a hidden declaration. @reexport */
export type Named = Value;
/** A second name for it. */
export type Renamed = Named;
/** Read by the hidden name. */
export function read(input: Value): Renamed {
	return input;
}

/** Plays against a clock, rendered before it, so its \`sync\` option must not take the clock's anchor. */
export class Player {
	constructor(options: { sync: Sync }) {
		void options;
	}
}
/** A clock. */
export class Sync {}
`,
				"node_modules/dep/package.json": JSON.stringify({ name: "dep", version: "1.0.0", types: "index.d.ts" }),
				"node_modules/dep/index.d.ts": `/** A dependency's public value. */
export declare class Thing {}
`,
			},
		},
	});

	test("each entrypoint gets a page named by its import specifier", () => {
		expect(page("owner.md")).toContain("# @test/owner");
		expect(page("owner/sub.md")).toContain("# @test/owner/sub");
		expect(page("owner/sub.md")).toContain("A value only the subpath exports.");
	});

	test("a re-export from another workspace package links to its owner", () => {
		expect(page("user.md")).toMatch(/Re-exports \[Thing\]\(owner\.md#thing\)/);
		expect(page("user.md")).not.toContain("A thing the owner defines.");
	});

	test("an external re-export is documented under each public name", () => {
		const user = page("user.md");
		expect(user).toContain("### First {#first}");
		expect(user).toContain("A dependency's public value.");
		expect(user).toMatch(/### Second \{#second\}\n\nRenames and re-exports \[First\]\(#first\)/);
	});

	test("a namespace export keeps its public name", () => {
		const user = page("user.md");
		expect(user).toContain("### Error {#error}");
		expect(user).toContain("Return the builtin error.");
		expect(user).not.toContain("Error_2");
	});

	test("a hidden declaration is documented under the alias that re-exports it", () => {
		const user = page("user.md");
		expect(user).toMatch(/### Named \{#named\}\n\n.*\n\nThe declaration\./);
		expect(user).toContain("Its text.");
		expect(user).toContain("[`Named`](#named)");
		expect(user).not.toContain("Value");
	});
});

test("an unexported declaration that a public one names fails the run", async () => {
	const files = {
		"src/index.ts": `export { read } from "./access";
/** A public value. */
export interface Value {
	/** Its text. */
	text: string;
}
`,
		"src/access.ts": `/** A hidden value sharing the public one's name. */
interface Value {
	/** Its count. */
	count: number;
}
/** Read the hidden value. */
export function read(): Value {
	return { count: 0 };
}
`,
	};
	await expect(render({ hidden: { files } })).rejects.toThrow(/^TypeDoc: 0 errors and \d+ warnings$/);
});

test("a link that does not resolve fails the run", async () => {
	const files = { "src/index.ts": "/** See {@link Missing}. */\nexport const value = 1;\n" };
	await expect(render({ broken: { files } })).rejects.toThrow(/^TypeDoc: 0 errors and \d+ warnings$/);
});

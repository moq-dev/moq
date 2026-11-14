import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fingerprint } from "./fingerprint";

const roots: string[] = [];
afterAll(() => {
	for (const root of roots) rmSync(root, { recursive: true, force: true });
});

/** Write a one-package workspace whose source reaches outside the package, returning its key and a writer. */
function workspace() {
	const root = mkdtempSync(join(tmpdir(), "moq-doc-fingerprint-"));
	roots.push(root);
	const write = (path: string, text: string) => {
		mkdirSync(dirname(join(root, path)), { recursive: true });
		writeFileSync(join(root, path), text);
	};
	write("tsconfig.json", JSON.stringify({ compilerOptions: { strict: true, lib: ["ES2022"] } }));
	write("pkg/package.json", JSON.stringify({ name: "@test/pkg", exports: { ".": "./src/index.ts" } }));
	write("pkg/tsconfig.json", JSON.stringify({ extends: "../tsconfig.json", include: ["src"] }));
	write("pkg/src/index.ts", 'export * from "./dep";\nexport * from "../../shared";\n');
	write("pkg/src/dep/index.ts", "export const dep = 1;\n");
	write("pkg/README.md", "# pkg\n");
	write("shared.ts", "export const shared = 1;\n");
	const key = () => fingerprint({ root, packages: [join(root, "pkg")] }, []);
	return { key, write };
}

test("a checkout elsewhere gets the same key", () => {
	expect(workspace().key()).toBe(workspace().key());
});

test("a file only the program reaches changes the key", () => {
	for (const path of ["shared.ts", "tsconfig.json"]) {
		const { key, write } = workspace();
		const before = key();
		write(path, "{}\n");
		expect(key()).not.toBe(before);
	}
});

test("a new file where module resolution looks changes the key", () => {
	const { key, write } = workspace();
	const before = key();
	write("pkg/src/dep.ts", "export const dep = 2;\n");
	expect(key()).not.toBe(before);
});

test("a file the reference never reads keeps the key", () => {
	const { key, write } = workspace();
	const before = key();
	write("pkg/README.md", "# changed\n");
	write("pkg/bench/run.ts", "export {};\n");
	expect(key()).toBe(before);
});

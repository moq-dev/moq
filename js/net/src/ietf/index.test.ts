import { expect, test } from "bun:test";
import { dirname, join, relative } from "node:path";

// Lite is preferred, so a lite session must not load the IETF session code: the connection
// imports it on demand. A static import anywhere on the entry's graph puts it in the first load.
test("the package entry loads the IETF session code on demand", async () => {
	const src = join(import.meta.dir, "..");
	const transpiler = new Bun.Transpiler({ loader: "ts" });
	const reached = new Set<string>();
	const pending = [join(src, "index.ts")];
	for (let file = pending.pop(); file !== undefined; file = pending.pop()) {
		if (reached.has(file)) continue;
		reached.add(file);
		for (const { kind, path } of transpiler.scanImports(await Bun.file(file).text())) {
			if (kind === "import-statement" && path.startsWith(".")) pending.push(join(dirname(file), path));
		}
	}

	const eager = [...reached].map((file) => relative(src, file));
	expect(eager).toContain("ietf/setup.ts");
	for (const lazy of ["ietf/connection.ts", "ietf/request_stream.ts"]) expect(eager).not.toContain(lazy);
});

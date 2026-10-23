import { expect, test } from "bun:test";
import { IMPLEMENTATION } from "./implementation.ts";

test("IMPLEMENTATION names the package version", async () => {
	const pkg = await Bun.file(new URL("../package.json", import.meta.url)).json();
	expect(IMPLEMENTATION).toBe(`moq-net-js/${pkg.version}`);
});

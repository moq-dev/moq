import { expect, test } from "bun:test";
import { Warmup } from "./warmup";

test("decoder warm-up restarts for each codec epoch", () => {
	const warmup = new Warmup(3);

	expect([warmup.drop(), warmup.drop(), warmup.drop(), warmup.drop()]).toEqual([true, true, true, false]);
	warmup.reset();
	expect([warmup.drop(), warmup.drop(), warmup.drop(), warmup.drop()]).toEqual([true, true, true, false]);
});

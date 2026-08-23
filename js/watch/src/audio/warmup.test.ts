import { expect, test } from "bun:test";
import { Warmup } from "./warmup";

test("decoder warm-up drops only the configured initial callbacks", () => {
	const warmup = new Warmup(3);

	expect([warmup.drop(), warmup.drop(), warmup.drop(), warmup.drop(), warmup.drop()]).toEqual([
		true,
		true,
		true,
		false,
		false,
	]);
});

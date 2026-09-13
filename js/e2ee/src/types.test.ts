import { expect, test } from "bun:test";
import type { Track } from "@moq/net";
import type { Consumer, Producer } from "./index.ts";

test("track wrappers take one options object", () => {
	const arities = [
		1 as typeof Producer.create extends (options: infer _) => unknown ? 1 : 0,
		1 as typeof Consumer.create extends (options: infer _) => unknown ? 1 : 0,
	];
	expect(arities).toEqual([1, 1]);

	const track: ConstructorParameters<typeof Track.Producer>[0] = "physical";
	expect(track).toBe("physical");
});

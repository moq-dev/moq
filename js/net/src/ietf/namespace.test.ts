import { expect, test } from "bun:test";
import { fromTuple, reserved } from "./namespace.ts";
import { Version } from "./version.ts";

test("reserved namespaces preserve tuple fields and draft boundaries", () => {
	for (const [fields, expected] of [
		[["."], true],
		[[".", "child"], true],
		[[".session"], true],
		[[".session", "extension"], true],
		[[".session/extension"], false],
		[["", ".session"], false],
		[[".application"], false],
		[[".sessionish"], false],
	] as [string[], boolean][]) {
		const path = fromTuple(fields);
		expect(reserved(path, Version.DRAFT_22)).toBe(expected);
		expect(reserved(path, Version.DRAFT_18)).toBe(false);
	}
});

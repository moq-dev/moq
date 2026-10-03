import { expect, test } from "bun:test";
import { matrix, newest, versions } from "./resolve";

test("a yanked or prerelease version cannot replace the installable release", () => {
	expect(
		newest([
			{ version: "1.9.0" },
			{ version: "1.10.0" },
			{ version: "2.0.0", yanked: true },
			{ version: "3.0.0-beta.1" },
		]),
	).toBe("1.10.0");
});

test("wrapped help adds drafts without a harness edit", () => {
	expect(
		versions("--connect-version value\n [possible values: moq-lite-06,\n moq-transport-99]\n--other value"),
	).toEqual(["moq-lite-06", "moq-transport-99"]);
	expect(() => versions("--connect-version unrestricted")).toThrow();
});

test("removing a published draft fails, while current-only drafts are logged", () => {
	expect(() => matrix(["new"], ["old"], {})).toThrow("removed published");
	expect(matrix(["old", "new"], ["old"], {})).toEqual({ shared: ["old"], currentOnly: ["new"], planned: {} });
	const exception = { reason: "Approved migration", releases: { "moq-cli": "1.0.0" } };
	expect(matrix(["new"], ["old"], { old: exception }, { "moq-cli": "1.0.0" }).planned).toEqual({ old: exception });
	expect(() => matrix(["old"], ["old"], { old: exception }, { "moq-cli": "1.0.1" })).toThrow("stale");
});

test("an approved wire break can retain its protocol name, but new drafts still fail", () => {
	const exception = { reason: "Unpublished wire changed", releases: { "@moq/net": "0.4.1" } };
	expect(matrix(["wip"], ["wip"], { wip: exception }, { "@moq/net": "0.4.1" }).shared).toEqual([]);
	expect(() => matrix(["wip"], ["wip", "unreviewed"], { wip: exception }, { "@moq/net": "0.4.1" })).toThrow(
		"removed published",
	);
});

import { expect, test } from "bun:test";
import { matrix, newest, planned, versions } from "./resolve";

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

const OLD = "moq-lite-01";
const NEW = "moq-lite-02";

test("removing a published draft fails, while current-only drafts are logged", () => {
	expect(() => matrix([NEW], [OLD], {})).toThrow("removed published");
	expect(matrix([OLD, NEW], [OLD], {})).toEqual({ shared: [OLD], currentOnly: [NEW], planned: {} });
	const exception = { reason: "Approved migration", releases: { "moq-cli": "1.0.0" } };
	expect(matrix([NEW], [OLD], { [OLD]: exception }, { "moq-cli": "1.0.0" }).planned).toEqual({ [OLD]: exception });
	expect(() => matrix([OLD], [OLD], { [OLD]: exception }, { "moq-cli": "1.0.1" })).toThrow("stale");
});

test("an approved wire break can retain its protocol name, but new drafts still fail", () => {
	const exception = { reason: "Unpublished wire changed", releases: { "@moq/net": "0.4.1" } };
	expect(matrix([OLD], [OLD], { [OLD]: exception }, { "@moq/net": "0.4.1" }).shared).toEqual([]);
	expect(() => matrix([OLD], [OLD, NEW], { [OLD]: exception }, { "@moq/net": "0.4.1" })).toThrow(
		"removed published",
	);
});

test("IETF drafts never enter the released comparison", () => {
	expect(matrix([OLD, "moq-transport-14"], [OLD, "moq-transport-14", "moq-transport-15"], {}).shared).toEqual([OLD]);
});

test("a cell-scoped break skips only its cells and expires with its releases", () => {
	const breaks = {
		hang: {
			reason: "Fetch hangs",
			releases: { "moq-relay": "1.0.0" },
			cells: { lanes: ["fetch"], versions: [OLD], relay: "released" as const },
		},
	};
	expect(matrix([OLD], [OLD], breaks, { "moq-relay": "1.0.0" }).shared).toEqual([OLD]);
	expect(() => matrix([OLD], [OLD], breaks, { "moq-relay": "1.0.1" })).toThrow("stale");
	expect(() => matrix([NEW], [NEW], breaks, { "moq-relay": "1.0.0" })).toThrow("stale");
	const cell = { lane: "fetch", version: OLD, relay: "released", publisher: "current" };
	expect(planned(breaks, cell)).toBe("hang");
	expect(planned(breaks, { ...cell, relay: "current" })).toBeUndefined();
	expect(planned(breaks, { ...cell, lane: "media" })).toBeUndefined();
});

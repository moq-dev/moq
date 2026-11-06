import { expect, test } from "bun:test";
import { type Measured, matrix, mediaControls, newest, planned, remeasure, stale, versions } from "./resolve";

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
	expect(matrix([NEW], [OLD], { [OLD]: exception }).planned).toEqual({ [OLD]: exception });
	expect(() => stale({ [OLD]: exception }, [OLD], { "moq-cli": "1.0.1" })).toThrow(
		"stale planned break: moq-lite-01, moq-cli changed from 1.0.0 to 1.0.1; run `just test wire-compat --remeasure`",
	);
});

test("an approved wire break can retain its protocol name, but new drafts still fail", () => {
	const exception = { reason: "Unpublished wire changed", releases: { "@moq/net": "0.4.1" } };
	expect(matrix([OLD], [OLD], { [OLD]: exception }).shared).toEqual([OLD]);
	const cell = { lane: "fetch", version: OLD, relay: "current", publisher: "released" };
	expect(planned({ [OLD]: exception }, cell)).toEqual([OLD]);
	// The FETCH capability check is a cell too, so the break covers it.
	expect(
		planned({ [OLD]: exception }, { lane: "fetch-capability", version: OLD, relay: "both", publisher: "both" }),
	).toEqual([OLD]);
	expect(planned({ [OLD]: exception }, { ...cell, version: NEW })).toEqual([]);
	expect(() => matrix([OLD], [OLD, NEW], { [OLD]: exception })).toThrow("removed published");
});

test("IETF drafts never enter the released comparison", () => {
	expect(matrix([OLD, "moq-transport-18"], [OLD, "moq-transport-14", "moq-transport-18"], {}).shared).toEqual([OLD]);
});

test("a cell-scoped break skips only its cells and expires with its releases", () => {
	const breaks = {
		hang: {
			reason: "Fetch hangs",
			releases: { "moq-relay": "1.0.0" },
			cells: { lanes: ["fetch"], versions: [OLD], relay: "released" as const },
		},
	};
	expect(matrix([OLD], [OLD], breaks).shared).toEqual([OLD]);
	expect(() => stale(breaks, [OLD], { "moq-relay": "1.0.0" })).not.toThrow();
	expect(() => stale(breaks, [OLD], { "moq-relay": "1.0.1" })).toThrow("stale");
	expect(() => stale(breaks, [NEW], { "moq-relay": "1.0.0" })).toThrow("moq-lite-01 is no longer released");
	const cell = { lane: "fetch", version: OLD, relay: "released", publisher: "current" };
	expect(planned(breaks, cell)).toEqual(["hang"]);
	expect(planned(breaks, { ...cell, relay: "current" })).toEqual([]);
	expect(planned(breaks, { ...cell, lane: "media" })).toEqual([]);
});

test("media controls always use a published lite baseline on both sources", () => {
	expect(mediaControls(["moq-lite-06", "moq-lite-07-wip"], ["moq-lite-06"])).toEqual([
		{ source: "current", version: "moq-lite-06" },
		{ source: "released", version: "moq-lite-06" },
	]);
	expect(() => mediaControls(["moq-lite-07-wip"], ["moq-lite-06"])).toThrow("media controls require");
});

test("planned breaks never skip same-source media controls", () => {
	const breaks = {
		archive: { reason: "catalog break", releases: { hang: "1.0.0" }, cells: { lanes: ["media", "media-control"] } },
	};
	for (const source of ["current", "released"]) {
		expect(
			planned(breaks, { lane: "media-control", version: "moq-lite-06", relay: source, publisher: source }),
		).toEqual([]);
	}
	expect(() =>
		planned(breaks, { lane: "media-control", version: "moq-lite-06", relay: "current", publisher: "released" }),
	).toThrow("one source");
});

test("a re-measure re-pins each break that still fails and deletes the rest", () => {
	const breaks = {
		[OLD]: { reason: "wire changed", releases: { "moq-cli": "1.0.0" } },
		archive: {
			reason: "catalog break",
			releases: { "@moq/hang": "1.0.0" },
			cells: { lanes: ["media"], versions: [OLD, NEW] },
		},
		fixed: { reason: "fetch hang", releases: { "moq-relay": "1.0.0" }, cells: { lanes: ["fetch"] } },
		unused: { reason: "no such lane", releases: { "moq-relay": "1.0.0" }, cells: { lanes: ["gone"] } },
	};
	const releases = { "moq-cli": "1.0.1", "@moq/hang": "1.0.1", "moq-relay": "1.0.1" };
	const { breaks: result, report } = remeasure(
		breaks,
		[
			{ names: [OLD], cell: "a", passed: false },
			{ names: [OLD], cell: "b", passed: true },
			{ names: ["archive"], cell: "c", passed: false },
			{ names: ["fixed"], cell: "d", passed: true },
		],
		{ current: [OLD], released: [OLD, "moq-transport-14"], releases },
	);
	expect(result).toEqual({
		[OLD]: { reason: "wire changed", releases: { "moq-cli": "1.0.1" } },
		archive: {
			reason: "catalog break",
			releases: { "@moq/hang": "1.0.1" },
			cells: { lanes: ["media"], versions: [OLD] },
		},
	});
	expect(report).toEqual([
		`${OLD}: still fails in 1 of 2 cells; re-pinned\n  a`,
		"archive: still fails in 1 of 1 cells; re-pinned\n  c",
		"fixed: no longer fails in 1 cells; deleted",
		"unused: covers no cells, deleted",
	]);
	// The re-pinned file passes the check a scheduled run makes.
	expect(() => stale(result, [OLD], releases)).not.toThrow();
});

test("a re-measure keeps a protocol break while the checkout lacks its version", () => {
	const breaks = { [OLD]: { reason: "removed", releases: { "moq-cli": "1.0.0" } } };
	const releases = { "moq-cli": "1.0.1" };
	expect(remeasure(breaks, [], { current: [NEW], released: [OLD], releases }).breaks).toEqual({
		[OLD]: { reason: "removed", releases },
	});
	expect(remeasure(breaks, [], { current: [NEW], released: [NEW], releases }).breaks).toEqual({});
	expect(() => remeasure(breaks, [], { current: [NEW], released: [OLD], releases: {} })).toThrow("does not resolve");
});

test("a failing cell two breaks cover keeps one of them", () => {
	const breaks = {
		[NEW]: { reason: "wire changed", releases: { "moq-cli": "1.0.0" } },
		archive: { reason: "catalog break", releases: { "@moq/hang": "1.0.0" }, cells: { lanes: ["media"] } },
	};
	const cell = { lane: "media", version: NEW, relay: "current", publisher: "current" };
	expect(planned(breaks, cell)).toEqual([NEW, "archive"]);
	const releases = { "moq-cli": "1.0.1", "@moq/hang": "1.0.1" };
	const context = { current: [OLD, NEW], released: [OLD, NEW], releases };
	const kept = (measured: Measured[]) => {
		const { breaks: result } = remeasure(breaks, measured, context);
		// The rewritten file still skips every cell that failed.
		for (const m of measured) if (!m.passed) expect(m.names.some((name) => name in result)).toBe(true);
		return Object.keys(result);
	};
	const shared = { names: [NEW, "archive"], cell: "shared", passed: false };
	const archivePasses = { names: ["archive"], cell: "archive alone", passed: true };
	const wireFails = { names: [NEW], cell: "wire alone", passed: false };
	const wirePasses = { ...wireFails, passed: true };
	// Nothing says which break the shared failure belongs to, so both stay.
	expect(kept([shared])).toEqual([NEW, "archive"]);
	expect(kept([shared, archivePasses, wirePasses])).toEqual([NEW, "archive"]);
	// A break that fails on its own explains the shared failure, so the other goes.
	expect(kept([shared, archivePasses, wireFails])).toEqual([NEW]);
	expect(kept([{ ...shared, passed: true }, archivePasses, wirePasses])).toEqual([]);
	expect(() => matrix([OLD], [OLD], { "two words": breaks.archive })).toThrow("invalid planned break");
});

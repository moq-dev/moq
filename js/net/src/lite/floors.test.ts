import { expect, test } from "bun:test";
import * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import { type Claim, Floors, type View } from "./floors.ts";

const NODE = 7n;
const A: Claim = { instance: Instance.parse("a"), epoch: 0n, cost: 0n };

function carrier(prefix: string, revision: bigint | undefined, claims: [string, Claim][], hidden = false) {
	const view: View = {
		prefix: Path.from(prefix),
		hidden,
		revision,
		claims: new Map(claims.map(([path, claim]) => [Path.from(path), claim])),
	};
	return { view, reshow: () => {} };
}

test("a newer floor decides a claim only where its request certifies it", () => {
	const floors = new Floors();
	// Behind, and asking for hidden claims too.
	const lagging = carrier(
		"",
		0n,
		[
			["live/x", A],
			["live/.h", A],
		],
		true,
	);
	// Newer, without hidden claims: it withdrew live/x and says nothing of live/.h.
	const newer = carrier("", 1n, []);
	floors.join(NODE, lagging);
	floors.join(NODE, newer);
	expect(floors.shows(NODE, lagging.view, Path.from("live/x"))).toBeUndefined();
	expect(floors.shows(NODE, lagging.view, Path.from("live/.h"))).toBeDefined();
});

test("a claim presented at a stream's own prefix matches the newer view's claim above it", () => {
	const floors = new Floors();
	// Asking for live/a, it is shown the claim at live there.
	const narrow = carrier("live/a", 0n, [["live/a", A]]);
	const wide = carrier("", 1n, [["live", A]]);
	floors.join(NODE, narrow);
	floors.join(NODE, wide);
	expect(floors.shows(NODE, narrow.view, Path.from("live/a"))).toBeDefined();
});

test("an uncertified claim stands until a floor certifies its path", () => {
	const floors = new Floors();
	const loose = carrier("", undefined, [
		["live/x", A],
		["other", A],
	]);
	const scoped = carrier("live", 3n, []);
	floors.join(NODE, loose);
	expect(floors.shows(NODE, loose.view, Path.from("live/x"))).toBeDefined();
	floors.join(NODE, scoped);
	expect(floors.shows(NODE, loose.view, Path.from("live/x"))).toBeUndefined();
	expect(floors.shows(NODE, loose.view, Path.from("other"))).toBeDefined();
});

test("a shown claim takes the newest certifying view's cost", () => {
	const floors = new Floors();
	const lagging = carrier("", 0n, [["live/x", A]]);
	const newer = carrier("", 1n, [["live/x", { ...A, cost: 100n }]]);
	floors.join(NODE, lagging);
	floors.join(NODE, newer);
	expect(floors.shows(NODE, lagging.view, Path.from("live/x"))?.cost).toBe(100n);
});

test("a regressed route's certified view decides only while a live floor certifies the path", () => {
	const floors = new Floors();
	const route = carrier("", 1n, [["live/x", A]]);
	const view = route.view as { revision: bigint | undefined; claims: Map<Path.Valid, Claim> };
	floors.join(NODE, route);
	// Its next floor certifies nothing and adds live/y: no floor certifies live/y now.
	floors.regress(NODE, route);
	view.revision = undefined;
	view.claims.set(Path.from("live/y"), A);
	expect(floors.shows(NODE, route.view, Path.from("live/y"))).toBeDefined();
	// A lagging stream at revision 0 carries live/y too: the kept revision 1 says it does not exist.
	const lagging = carrier("", 0n, [["live/y", A]]);
	floors.join(NODE, lagging);
	expect(floors.shows(NODE, lagging.view, Path.from("live/y"))).toBeUndefined();
});

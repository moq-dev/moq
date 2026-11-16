// The same cases as `lite/rekey.rs` in rs/moq-net: both endpoints must carry routes alike.

import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import type { Offer } from "./announce.ts";
import { compare, type Order, order, type Plan, plan, type Route } from "./rekey.ts";

const BEST: Order = { held: false, names: false, draining: false, priority: 0, metric: 1n };
const HELD: Order = { ...BEST, held: true };

const set = (...paths: string[]) => new Set(paths.map((path) => Path.from(path)));

function route(node: bigint, prefix: string, region: string, rank: Order, claims: [string, bigint][]): Route {
	return {
		node,
		prefix: Path.from(prefix),
		region: Path.from(region),
		exact: false,
		order: rank,
		claims: new Map(claims.map(([path, id]) => [Path.from(path), id])),
	};
}

const ids = (carried: Plan) => carried.routes.map((r) => [r.id, r.from, r.kind, r.prefix as string]);
const claims = (carried: Plan, index: number) =>
	[...carried.routes[index].claims].map(([path, claim]) => [path as string, claim.id, claim.from]).sort();

test("widening merges and keeps ids", () => {
	const routes = new Map([
		[
			0n,
			route(7n, "a/b", "a/b", HELD, [
				["a/b", 0n],
				["a/b/x", 1n],
			]),
		],
		[1n, route(7n, "a/c", "a/c", BEST, [["a/c/y", 2n]])],
		[2n, route(7n, "a/c/deep", "a/c", BEST, [["a/c/deep/z", 3n]])],
		[3n, route(9n, "q", "q", BEST, [["q/k", 4n]])],
	]);
	const carried = plan(set("a/b", "a/c", "q"), set("a"), [Path.from("a")], routes, 4n, 5n);
	expect(ids(carried)).toEqual([
		[1n, 1n, "widens", "a"],
		[2n, 2n, "stays", "a/c/deep"],
	]);
	// The claim at the route's own prefix goes; the loser's claims move in.
	expect(claims(carried, 0)).toEqual([
		["a/b/x", 1n, 1n],
		["a/c/y", 2n, 2n],
	]);
	expect(carried.routes[1].region).toBe(Path.from("a"));
	expect([carried.nextRoute, carried.nextClaim]).toEqual([4n, 5n]);
});

test("narrowing presents the covering claim", () => {
	const routes = new Map([
		[
			0n,
			route(7n, "a", "a", BEST, [
				["a", 0n],
				["a/b/x", 1n],
				["a/c", 2n],
				["a/d", 3n],
			]),
		],
		[1n, route(7n, "a/c", "a", HELD, [["a/c/y", 4n]])],
	]);
	const added = [Path.from("a/c"), Path.from("a/b")];
	const carried = plan(set("a"), set("a/b", "a/c"), added, routes, 2n, 5n);
	// The narrowed route is preferred to the held one already at `a/c`.
	expect(ids(carried)).toEqual([
		[2n, 0n, "narrows", "a/c"],
		[3n, 0n, "narrows", "a/b"],
	]);
	expect(claims(carried, 0)).toEqual([["a/c", 2n, 2n]]);
	expect(claims(carried, 1)).toEqual([
		["a/b", 5n, 0n],
		["a/b/x", 1n, 1n],
	]);
	// Only the claim now presented below its path is further above it, by the segments between.
	const lowered = (index: number) =>
		[...carried.routes[index].claims].map(([path, claim]) => [path as string, claim.lowered]).sort();
	expect(lowered(0)).toEqual([["a/c", 0n]]);
	expect(lowered(1)).toEqual([
		["a/b", 1n],
		["a/b/x", 0n],
	]);
	expect([carried.nextRoute, carried.nextClaim]).toEqual([4n, 6n]);
});

test("a staying route keeps its key against an equal", () => {
	const routes = new Map([
		[0n, route(7n, "a", "a", BEST, [["a/c/y", 1n]])],
		[1n, route(7n, "a/c", "a", BEST, [["a/c/z", 2n]])],
		[2n, route(7n, "b", "b", BEST, [])],
	]);
	const carried = plan(set("a", "b"), set("a/c", "b"), [Path.from("a/c")], routes, 3n, 3n);
	expect(ids(carried)).toEqual([
		[1n, 1n, "stays", "a/c"],
		[2n, 2n, "stays", "b"],
	]);
});

test("a re-added prefix keeps its routes, and an uncovered one ends", () => {
	const routes = new Map([
		[0n, route(7n, "a", "a", BEST, [["a/x", 0n]])],
		[1n, route(7n, "b", "b", BEST, [["b/y", 1n]])],
	]);
	const carried = plan(set("a", "b"), set("a"), [Path.from("a")], routes, 2n, 2n);
	expect(ids(carried)).toEqual([[0n, 0n, "stays", "a"]]);
	expect([carried.nextRoute, carried.nextClaim]).toEqual([2n, 2n]);
});

test("a route ranks unheld, then off the subscriber's path, then by its offer", () => {
	const offer = (extra: Partial<Offer>): Offer => ({
		path: [{ node: 7n, session: 0n }],
		draining: false,
		priority: 0,
		metric: 1n,
		downOnly: false,
		...extra,
	});
	const ranked = [
		order(false, offer({ priority: 5, metric: 9n }), [{ node: 7n, session: 0n }], 3n),
		order(false, offer({}), [{ node: 7n, session: 0n }], 3n),
		order(false, offer({ metric: 2n }), [{ node: 7n, session: 0n }], 3n),
		order(false, offer({ draining: true }), [{ node: 7n, session: 0n }], 3n),
		// A refused offer ranks behind every usable one.
		order(false, undefined, [{ node: 7n, session: 0n }], 3n),
		order(false, offer({}), [{ node: 3n, session: 0n }], 3n),
		order(true, offer({}), [{ node: 7n, session: 0n }], 3n),
	];
	for (let i = 0; i < ranked.length; i++) {
		for (let j = 0; j < ranked.length; j++) {
			expect(Math.sign(compare(ranked[i], ranked[j]))).toBe(Math.sign(i - j));
		}
	}
});

test("an exact route stays at its own prefix", () => {
	const exact = (r: Route): Route => ({ ...r, exact: true });
	let routes = new Map([
		[
			0n,
			exact(
				route(7n, "a/b", "a/b", BEST, [
					["a/b", 0n],
					["a/b/x", 1n],
				]),
			),
		],
		[
			1n,
			route(8n, "a/b", "a/b", BEST, [
				["a/b", 2n],
				["a/b/y", 3n],
			]),
		],
	]);
	let carried = plan(set("a/b"), set("a"), [Path.from("a")], routes, 2n, 4n);
	expect(ids(carried)).toEqual([
		[0n, 0n, "stays", "a/b"],
		[1n, 1n, "widens", "a"],
	]);
	expect([carried.routes[0].region, carried.routes[0].exact]).toEqual([Path.from("a"), true]);
	expect(claims(carried, 0)).toEqual([
		["a/b", 0n, 0n],
		["a/b/x", 1n, 1n],
	]);
	expect(carried.routes[1].exact).toBe(false);

	// Asked at its own prefix again, it lies there exactly once more.
	routes = new Map([[0n, exact(route(7n, "a/b", "a", BEST, []))]]);
	carried = plan(set("a"), set("a/b"), [Path.from("a/b")], routes, 2n, 4n);
	expect(ids(carried)).toEqual([[0n, 0n, "stays", "a/b"]]);
	expect([carried.routes[0].region, carried.routes[0].exact]).toEqual([Path.from("a/b"), true]);

	// Narrowed, it covers the narrower prefix from its own.
	routes = new Map([[0n, exact(route(7n, "a/b", "a/b", BEST, [["a/b", 0n]]))]]);
	carried = plan(set("a/b"), set("a/b/c"), [Path.from("a/b/c")], routes, 1n, 1n);
	expect(ids(carried)).toEqual([[1n, 0n, "narrows", "a/b/c"]]);
	expect(carried.routes[0].exact).toBe(false);
});

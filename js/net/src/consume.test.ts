import { expect, test } from "bun:test";
import { Producer } from "./broadcast.ts";
import { BroadcastCache } from "./consume.ts";
import * as Epoch from "./epoch.ts";
import { Route } from "./hop.ts";
import * as Path from "./path.ts";

test("cache identity follows the serving announcement, preserving held tracks", async () => {
	const cache = new BroadcastCache();
	const pool = Path.from("pool");
	const job = Path.from("pool/job");
	const broad = {};
	const specific = {};
	const epoch = Epoch.mint();
	cache.announce(pool, broad, { ...Route.default, epoch });
	const source = new Producer();
	const track = source.createTrack("video");
	const held = cache.insert(job, cache.instance(job), source.consume());
	const ordered = held.track("video").subscribe().ordered();
	cache.announce(job, specific, { ...Route.default, epoch });
	const same = cache.get(job, cache.instance(job));
	expect(same?.closed).toBe(held.closed);

	cache.announce(job, specific, Route.default, true);
	expect(cache.instance(job).route.epoch).toBeUndefined();
	expect(cache.get(job, cache.instance(job))).toBeUndefined();
	const replacement = new Producer();
	const fresh = cache.insert(job, cache.instance(job), replacement.consume());
	cache.withdraw(pool, broad);
	const shared = cache.get(job, cache.instance(job));
	expect(shared?.closed).toBe(fresh.closed);
	// Cache replacement never transfers or closes the track an old handle is reading.
	track.appendGroup().close();
	expect((await ordered.nextGroup())?.sequence).toBe(0);
	cache.withdraw(job, specific);
	expect(cache.get(job, cache.instance(job))).toBeUndefined();
	for (const consumer of [held, same, fresh, shared]) consumer?.close();
	ordered.close();
	source.close();
	replacement.close();
});

test("withdrawing one interest keeps another interest's explicit identity", () => {
	const cache = new BroadcastCache();
	const path = Path.from("room");
	const first = {};
	const second = {};
	const route = { ...Route.default, epoch: Epoch.mint() };
	cache.announce(path, first, route);
	cache.announce(path, second, route);
	const source = new Producer();
	const held = cache.insert(path, cache.instance(path), source.consume());
	cache.withdraw(path, first);
	const same = cache.get(path, cache.instance(path));
	expect(same?.closed).toBe(held.closed);
	cache.withdraw(path, second);
	expect(cache.get(path, cache.instance(path))).toBeUndefined();
	held.close();
	same?.close();
	source.close();
});

test("an announcement lifecycle retires a blind cache without disturbing unrelated paths", () => {
	const cache = new BroadcastCache();
	const path = Path.from("pool/job");
	const other = Path.from("other/job");
	const source = new Producer();
	const unrelated = new Producer();
	const held = cache.insert(path, cache.instance(path), source.consume());
	const otherHeld = cache.insert(other, cache.instance(other), unrelated.consume());
	const owner = {};
	cache.announce(Path.from("pool"), owner, Route.default);
	cache.withdraw(Path.from("pool"), owner);
	expect(cache.get(path, cache.instance(path))).toBeUndefined();
	const same = cache.get(other, cache.instance(other));
	expect(same?.closed).toBe(otherHeld.closed);
	expect(held.closed.peek()).toBeUndefined();
	for (const consumer of [held, otherHeld, same]) consumer?.close();
	source.close();
	unrelated.close();
});

test.each([false, true])("route alternation reuses every held instance (epoch: %s)", (explicit) => {
	const cache = new BroadcastCache();
	const path = Path.from("pool/job");
	const broad = Path.from("pool");
	const ownerA = {};
	const ownerB = {};
	cache.announce(broad, ownerA, { ...Route.default, epoch: explicit ? Epoch.mint() : undefined });
	const a = new Producer();
	const heldA = cache.insert(path, cache.instance(path), a.consume());
	cache.announce(path, ownerB, { ...Route.default, epoch: explicit ? Epoch.mint() : undefined });
	const b = new Producer();
	const heldB = cache.insert(path, cache.instance(path), b.consume());
	cache.withdraw(path, ownerB);
	const again = cache.get(path, cache.instance(path));
	expect(again?.closed).toBe(heldA.closed);
	for (const consumer of [heldA, heldB, again]) consumer?.close();
	a.close();
	b.close();
});

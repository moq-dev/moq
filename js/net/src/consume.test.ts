import { expect, test } from "bun:test";
import { Producer } from "./broadcast.ts";
import { BroadcastCache } from "./consume.ts";
import * as Epoch from "./epoch.ts";
import { StreamCode, StreamError } from "./error.ts";
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
	track.appendGroup().finish();
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

test("request validity cancels one selection period and permits the held fallback again", () => {
	const cache = new BroadcastCache();
	const path = Path.from("pool/job");
	const broad = Path.from("pool");
	const a = {};
	const b = {};
	cache.announce(broad, a, Route.default);
	const sourceA = new Producer();
	const instanceA = cache.instance(path);
	const heldA = cache.insert(path, instanceA, sourceA.consume());
	const first = instanceA.validity?.signal;
	expect(first?.aborted).toBe(false);
	cache.announce(path, b, Route.default);
	expect(first?.aborted).toBe(true);
	// A more specific route taking over ends the old instance's requests the way Rust does, so
	// players treat it as the instance going offline rather than an unexpected failure.
	expect(first?.reason).toBeInstanceOf(StreamError);
	expect(first?.reason).toMatchObject({ code: StreamCode.Unroutable });
	const sourceB = new Producer();
	const instanceB = cache.instance(path);
	const heldB = cache.insert(path, instanceB, sourceB.consume());
	expect(instanceB.validity?.signal.aborted).toBe(false);
	cache.withdraw(path, b);
	expect(instanceB.validity?.signal.aborted).toBe(true);
	expect(instanceA.validity?.signal).not.toBe(first);
	expect(instanceA.validity?.signal.aborted).toBe(false);
	expect(first?.aborted).toBe(true);
	expect(heldA.closed.peek()).toBeUndefined();
	expect(heldB.closed.peek()).toBeUndefined();
	heldA.close();
	heldB.close();
	sourceA.close();
	sourceB.close();
});

test("request validity follows the selected interest and ignores unrelated withdrawals", () => {
	const cache = new BroadcastCache();
	const path = Path.from("pool/job");
	const broad = {};
	const first = {};
	const second = {};
	cache.announce(Path.from("pool"), broad, Route.default);
	cache.announce(path, first, Route.default);
	cache.announce(path, second, Route.default);
	const source = new Producer();
	const instance = cache.instance(path);
	const held = cache.insert(path, instance, source.consume());
	const signal = instance.validity?.signal;
	cache.withdraw(Path.from("pool"), broad);
	cache.withdraw(path, second);
	cache.announce(path, first, { ...Route.default, cost: 1n });
	expect(instance.validity?.signal).toBe(signal);
	expect(signal?.aborted).toBe(false);
	cache.withdraw(path, first);
	expect(signal?.aborted).toBe(true);
	held.close();
	source.close();
});

test("a blind selection retires on discovery while explicit epochs need no path gate", () => {
	const cache = new BroadcastCache();
	const path = Path.from("pool/job");
	const blind = cache.instance(path);
	const source = new Producer();
	const held = cache.insert(path, blind, source.consume());
	const owner = {};
	cache.announce(Path.from("pool"), owner, { ...Route.default, epoch: Epoch.mint() });
	expect(blind.validity?.signal.aborted).toBe(true);
	expect(cache.instance(path).validity).toBeUndefined();
	cache.withdraw(Path.from("pool"), owner);
	expect(blind.validity?.signal.aborted).toBe(true);
	expect(cache.instance(path).identity).not.toBe(blind.identity);
	held.close();
	source.close();
});

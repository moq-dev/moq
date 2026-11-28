import { expect, test } from "bun:test";
import { Producer } from "./broadcast.ts";
import { BroadcastCache } from "./consume.ts";
import { defined } from "./defined.test-util.ts";
import { StreamCode, StreamError } from "./error.ts";
import { WireRoute } from "./hop.ts";
import * as Identity from "./identity.ts";
import { hooks } from "./internal.ts";
import * as Path from "./path.ts";

test("cache identity follows the serving announcement, preserving held tracks", async () => {
	const cache = new BroadcastCache();
	const pool = Path.from("pool");
	const job = Path.from("pool/job");
	const broad = {};
	const specific = {};
	const identity = Identity.random();
	cache.announce(pool, broad, { ...WireRoute.default, identity });
	const source = new Producer();
	const track = defined(source.createTrack("video"));
	const held = cache.insert(job, cache.identity(job), source.consume());
	const ordered = held.track("video").subscribe().ordered();
	cache.announce(job, specific, { ...WireRoute.default, identity });
	const same = cache.get(job, cache.identity(job));
	expect(same?.closed).toBe(held.closed);

	cache.announce(job, specific, WireRoute.default, true);
	expect(cache.identity(job).route.identity).toBeUndefined();
	expect(cache.get(job, cache.identity(job))).toBeUndefined();
	const replacement = new Producer();
	const fresh = cache.insert(job, cache.identity(job), replacement.consume());
	cache.withdraw(pool, broad);
	const shared = cache.get(job, cache.identity(job));
	expect(shared?.closed).toBe(fresh.closed);
	// Cache replacement never transfers or closes the track an old handle is reading.
	hooks.appendNext(track).finish();
	expect((await ordered.nextGroup())?.start).toBe(0n);
	cache.withdraw(job, specific);
	expect(cache.get(job, cache.identity(job))).toBeUndefined();
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
	const route = { ...WireRoute.default, identity: Identity.random() };
	cache.announce(path, first, route);
	cache.announce(path, second, route);
	const source = new Producer();
	const held = cache.insert(path, cache.identity(path), source.consume());
	cache.withdraw(path, first);
	const same = cache.get(path, cache.identity(path));
	expect(same?.closed).toBe(held.closed);
	cache.withdraw(path, second);
	expect(cache.get(path, cache.identity(path))).toBeUndefined();
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
	const held = cache.insert(path, cache.identity(path), source.consume());
	const otherHeld = cache.insert(other, cache.identity(other), unrelated.consume());
	const owner = {};
	cache.announce(Path.from("pool"), owner, WireRoute.default);
	cache.withdraw(Path.from("pool"), owner);
	expect(cache.get(path, cache.identity(path))).toBeUndefined();
	const same = cache.get(other, cache.identity(other));
	expect(same?.closed).toBe(otherHeld.closed);
	expect(held.closed.peek()).toBeUndefined();
	for (const consumer of [held, otherHeld, same]) consumer?.close();
	source.close();
	unrelated.close();
});

test.each([false, true])("route alternation reuses every held identity (identity: %s)", (explicit) => {
	const cache = new BroadcastCache();
	const path = Path.from("pool/job");
	const broad = Path.from("pool");
	const ownerA = {};
	const ownerB = {};
	cache.announce(broad, ownerA, { ...WireRoute.default, identity: explicit ? Identity.random() : undefined });
	const a = new Producer();
	const heldA = cache.insert(path, cache.identity(path), a.consume());
	cache.announce(path, ownerB, { ...WireRoute.default, identity: explicit ? Identity.random() : undefined });
	const b = new Producer();
	const heldB = cache.insert(path, cache.identity(path), b.consume());
	cache.withdraw(path, ownerB);
	const again = cache.get(path, cache.identity(path));
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
	cache.announce(broad, a, WireRoute.default);
	const sourceA = new Producer();
	const identityA = cache.identity(path);
	const heldA = cache.insert(path, identityA, sourceA.consume());
	const first = identityA.validity?.signal;
	expect(first?.aborted).toBe(false);
	cache.announce(path, b, WireRoute.default);
	expect(first?.aborted).toBe(true);
	// A more specific route taking over ends the old identity's requests the way Rust does, so
	// players treat it as the identity going offline rather than an unexpected failure.
	expect(first?.reason).toBeInstanceOf(StreamError);
	expect(first?.reason).toMatchObject({ code: StreamCode.Unroutable });
	const sourceB = new Producer();
	const identityB = cache.identity(path);
	const heldB = cache.insert(path, identityB, sourceB.consume());
	expect(identityB.validity?.signal.aborted).toBe(false);
	cache.withdraw(path, b);
	expect(identityB.validity?.signal.aborted).toBe(true);
	expect(identityA.validity?.signal).not.toBe(first);
	expect(identityA.validity?.signal.aborted).toBe(false);
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
	cache.announce(Path.from("pool"), broad, WireRoute.default);
	cache.announce(path, first, WireRoute.default);
	cache.announce(path, second, WireRoute.default);
	const source = new Producer();
	const identity = cache.identity(path);
	const held = cache.insert(path, identity, source.consume());
	const signal = identity.validity?.signal;
	// The newest of a tie at its own prefix is selected: `second`.
	cache.withdraw(Path.from("pool"), broad);
	cache.withdraw(path, first);
	cache.announce(path, second, { ...WireRoute.default, cost: 1n });
	expect(identity.validity?.signal).toBe(signal);
	expect(signal?.aborted).toBe(false);
	cache.withdraw(path, second);
	expect(signal?.aborted).toBe(true);
	held.close();
	source.close();
});

test("a blind selection retires on discovery while explicit identities need no path gate", () => {
	const cache = new BroadcastCache();
	const path = Path.from("pool/job");
	const blind = cache.identity(path);
	const source = new Producer();
	const held = cache.insert(path, blind, source.consume());
	const owner = {};
	cache.announce(Path.from("pool"), owner, { ...WireRoute.default, identity: Identity.random() });
	expect(blind.validity?.signal.aborted).toBe(true);
	expect(cache.identity(path).validity).toBeUndefined();
	cache.withdraw(Path.from("pool"), owner);
	expect(blind.validity?.signal.aborted).toBe(true);
	expect(cache.identity(path).ref).not.toBe(blind.ref);
	held.close();
	source.close();
});

// A tie at a path's own prefix goes to the newest stream, as the origin and Rust rank it, and a
// restart makes a stream the newest again; a re-price keeps its place.
test("the newest stream wins a tie at its own prefix", () => {
	const cache = new BroadcastCache();
	const path = Path.from("room");
	const older = {};
	const newer = {};
	const x = { ...WireRoute.default, identity: Identity.random() };
	const y = { ...WireRoute.default, identity: Identity.random() };
	cache.announce(path, older, x);
	cache.announce(path, newer, y);
	expect(cache.identity(path).route.identity).toBe(y.identity);
	cache.announce(path, older, { ...x, cost: 0n });
	expect(cache.identity(path).route.identity).toBe(y.identity);
	const z = { ...WireRoute.default, identity: Identity.random() };
	cache.announce(path, older, z, true);
	expect(cache.identity(path).route.identity).toBe(z.identity);
});

// A GOAWAY drains every claim of a session at once: a request on the claim selected before and
// after stays valid, though draining the claims one by one would pass the path through another.
test("a batch rechecks a path once, after its last change", () => {
	const cache = new BroadcastCache();
	const path = Path.from("room");
	const [cheap, dear] = [{}, {}];
	cache.announce(path, cheap, { ...WireRoute.default, cost: 1n });
	cache.announce(path, dear, { ...WireRoute.default, cost: 5n });
	const source = new Producer();
	const selected = cache.identity(path);
	const held = cache.insert(path, selected, source.consume());
	const signal = selected.validity?.signal;
	cache.batch(() => {
		cache.announce(path, cheap, { ...WireRoute.default, cost: 1n, draining: true });
		cache.announce(path, dear, { ...WireRoute.default, cost: 5n, draining: true });
	});
	expect(signal?.aborted).toBe(false);
	cache.announce(path, cheap, { ...WireRoute.default, cost: 1n, draining: true, available: false });
	expect(signal?.aborted).toBe(true);
	held.close();
	source.close();
});

test("a cached broadcast is reused only within the epoch it was consumed through", () => {
	const cache = new BroadcastCache();
	const path = Path.from("room");
	const owner = {};
	const identity = Identity.random();
	cache.announce(path, owner, { ...WireRoute.default, identity, epoch: 1n });
	const source = new Producer();
	const held = cache.insert(path, cache.identity(path), source.consume());
	cache.announce(path, owner, { ...WireRoute.default, identity, epoch: 2n });
	// A fresh request asks Epoch 2, which may refuse, rather than share Epoch 1's subscription.
	expect(cache.get(path, cache.identity(path))).toBeUndefined();
	const replacement = new Producer();
	const fresh = cache.insert(path, cache.identity(path), replacement.consume());
	const shared = cache.get(path, cache.identity(path));
	expect(shared?.closed).toBe(fresh.closed);
	// The Epoch-1 reader keeps its broadcast until it lets go.
	expect(held.closed.peek()).toBeUndefined();
	for (const consumer of [held, fresh, shared]) consumer?.close();
	source.close();
	replacement.close();
});

import { expect, test } from "bun:test";
import { getter } from "@moq/signals";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import type { Consumer } from "./origin.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

/**
 * The table's route for `path` as a handle of the caller's own, or undefined when nothing
 * routes it.
 *
 * A request is the only way to consume by path, and it resolves synchronously against the
 * table, so this is the whole of the one-shot lookup the origin used to expose. Cloned
 * because a request only borrows the table's front.
 */
function routed(consumer: Consumer, path: Path.Valid): BroadcastConsumer | undefined {
	const request = consumer.request(path);
	const front = request.active.peek()?.clone();
	request.close();
	return front;
}

test("a published broadcast resolves by path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const path = Path.from("room");
	expect(consumer.routes(path)).toBe(false);

	const broadcast = origin.publish(path);
	broadcast.createTrack("video");

	const handle = routed(consumer, path);
	expect(handle).toBeDefined();

	// The handle reaches the published tracks.
	const track = handle?.subscribe("video");
	expect(track).toBeDefined();
	track?.close();

	handle?.close();
	broadcast.close();
	origin.close();
});

test("closing the producer unpublishes the path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const broadcast = origin.publish(path);
	expect(consumer.routes(path)).toBe(true);

	broadcast.close();
	await settle();
	expect(consumer.routes(path)).toBe(false);

	origin.close();
});

test("a stale broadcast closing does not unpublish a republished path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const first = origin.publish(path);
	const second = origin.publish(path);

	// The republish already superseded it, so this close must not remove the live one.
	first.close();
	await settle();

	const handle = routed(consumer, path);
	expect(handle).toBeDefined();
	handle?.close();

	second.close();
	await settle();
	expect(consumer.routes(path)).toBe(false);

	origin.close();
});

test("a republish closes the superseded broadcast", async () => {
	const origin = new Producer();
	const path = Path.from("room");

	const first = origin.publish(path);
	origin.publish(path);

	await settle();
	// The origin held the only handle on the first broadcast, so superseding it closed it.
	expect(first.closed.peek()).not.toBeUndefined();

	origin.close();
});

test("a consumer clone keeps a superseded broadcast alive", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const first = origin.publish(path);
	const mine = routed(consumer, path);
	expect(mine).toBeDefined();

	origin.publish(path);
	await settle();

	// The application's clone holds the old broadcast open even though it is unpublished.
	expect(first.closed.peek()).toBeUndefined();

	mine?.close();
	await settle();
	expect(first.closed.peek()).not.toBeUndefined();

	origin.close();
});

test("closing the origin closes every routed broadcast", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const a = origin.publish(Path.from("a"));
	const b = origin.publish(Path.from("b"));

	const abort = new Error("shutdown");
	origin.close(abort);

	expect(origin.closed.peek()).toBe(abort);
	expect(consumer.closed.peek()).toBe(abort);
	expect(a.closed.peek()).toBe(abort);
	expect(b.closed.peek()).toBe(abort);

	expect(consumer.routes(Path.from("a"))).toBe(false);
	expect(() => origin.publish(Path.from("late"))).toThrow();

	// Idempotent: the first close wins.
	origin.close();
	expect(origin.closed.peek()).toBe(abort);
});

test("the table is reactive", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const changed = consumer.broadcasts.changed();
	const broadcast = origin.publish(path);

	const table = await changed;
	expect(table?.has(path)).toBe(true);

	broadcast.close();
	origin.close();
});

test("announced streams the table with prefix-relative paths", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const a = origin.publish(Path.from("room/a"));

	const announced = consumer.announced(Path.from("room"));

	// The initial state arrives first.
	expect(await announced.next()).toEqual({ path: Path.from("a"), active: true });

	// Additions under the prefix stream in; paths outside it are invisible.
	const b = origin.publish(Path.from("room/b"));
	origin.publish(Path.from("lobby/c"));
	expect(await announced.next()).toEqual({ path: Path.from("b"), active: true });

	// Removals retract.
	b.close();
	expect(await announced.next()).toEqual({ path: Path.from("b"), active: false });

	// The stream ends when the origin closes.
	origin.close();
	expect(await announced.next()).toBeUndefined();

	a.close();
});

test("a remote entry resolves by path and retracts on dispose", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("relayed");

	// Stand in for a session's discovered broadcast.
	const upstream = new BroadcastProducer();
	const dispose = origin.insertRemote(path, upstream.consume());

	const handle = routed(consumer, path);
	expect(handle).toBeDefined();
	handle?.close();

	// Announced streams include remote entries.
	const announced = consumer.announced();
	expect(await announced.next()).toEqual({ path, active: true });

	dispose();
	expect(await announced.next()).toEqual({ path, active: false });
	expect(consumer.routes(path)).toBe(false);

	announced.close();
	upstream.close();
	origin.close();
});

test("a local publish shadows a remote entry", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const upstream = new BroadcastProducer();
	upstream.createTrack("remote-track");
	const dispose = origin.insertRemote(path, upstream.consume());

	const local = origin.publish(path);
	local.createTrack("local-track");

	// Local wins: the handle reaches the local track, not the remote one.
	const handle = routed(consumer, path);
	const track = handle?.subscribe("local-track");
	expect(track).toBeDefined();
	track?.close();
	handle?.close();

	// One path, one announcement, even though both tables route it.
	const announced = consumer.announced();
	expect(await announced.next()).toEqual({ path, active: true });

	// Dropping the local publish falls back to the remote entry without a retraction.
	local.close();
	const back = routed(consumer, path);
	expect(back).toBeDefined();
	back?.close();

	announced.close();
	dispose();
	upstream.close();
	origin.close();
});

test("the publisher-facing table excludes remote entries", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	origin.publish(Path.from("mine"));
	const upstream = new BroadcastProducer();
	origin.insertRemote(Path.from("theirs"), upstream.consume());

	// What a session announces to a peer: local only, so a shared origin cannot echo.
	const table = consumer.broadcasts.peek();
	expect(table?.has(Path.from("mine"))).toBe(true);
	expect(table?.has(Path.from("theirs"))).toBe(false);

	upstream.close();
	origin.close();
});

test("inserting into a closed origin releases the front", async () => {
	const origin = new Producer();
	origin.close();

	const upstream = new BroadcastProducer();
	const front = upstream.consume();
	const dispose = origin.insertRemote(Path.from("late"), front);
	dispose();

	// The origin dropped the only handle, closing the broadcast.
	await settle();
	expect(upstream.closed.peek()).not.toBeUndefined();
});

test("a request resolves once a front answers, and survives its withdrawal", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("wanted");

	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();

	// A session answers (simulated): the slot's front resolves the request.
	const upstream = new BroadcastProducer();
	const slot = origin.requests.peek()?.get(path);
	expect(slot).toBeDefined();
	expect(origin.answer(path, upstream.consume())).toBeDefined();
	expect(request.active.peek()).toBeDefined();

	// A second request for the same path shares the answer, each through a handle of its
	// own, so one of them closing cannot take the other's subscription down.
	const again = consumer.request(path);
	expect(again.active.peek()).toBeDefined();
	expect(again.active.peek()).not.toBe(request.active.peek());
	again.close();
	expect(request.active.peek()).toBeDefined();

	// The last close withdraws the request and releases the front, a microtask later so an
	// effect rerun can re-acquire the slot without dropping the answer.
	request.close();
	await settle();
	expect(origin.requests.peek()?.has(path)).toBe(false);
	expect(upstream.closed.peek()).not.toBeUndefined();

	origin.close();
});

test("a request closed and retaken in the same tick keeps its answer", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("stable");

	const first = consumer.request(path);
	const upstream = new BroadcastProducer();
	const withdraw = origin.answer(path, upstream.consume());
	expect(withdraw).toBeDefined();
	const front = first.active.peek();
	expect(front).toBeDefined();

	// The pattern an effect produces when its rerun was triggered by the answer resolving:
	// cleanup closes the old request, the rerun takes a new one, all in one tick. The
	// answer must survive, or the subscription flaps and is re-dialed forever.
	first.close();
	const second = consumer.request(path);
	await settle();
	await settle();

	// A handle of the new request's own, but the same answer underneath: the slot kept it,
	// so the subscription was never re-dialed.
	expect(second.active.peek()).toBeDefined();
	expect(upstream.closed.peek()).toBeUndefined();

	second.close();
	origin.close();
});

test("disposing the newest remote route promotes the fallback", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("redundant");

	// Two sessions announced the same path; the older one is still alive when the newer
	// one goes away, so the route must fail over rather than black-hole.
	const older = new BroadcastProducer();
	older.createTrack("chat");
	const disposeOlder = origin.insertRemote(path, older.consume());

	const newer = new BroadcastProducer();
	const disposeNewer = origin.insertRemote(path, newer.consume());

	const announced = consumer.announced();
	expect(await announced.next()).toEqual({ path, active: true });

	// The newer session dies: consumers see a retract then the promoted fallback.
	disposeNewer();
	expect(await announced.next()).toEqual({ path, active: false });
	expect(await announced.next()).toEqual({ path, active: true });

	const handle = routed(consumer, path);
	const track = handle?.subscribe("chat");
	expect(track).toBeDefined();
	track?.close();
	handle?.close();

	// The older source was never the origin's to close.
	expect(older.closed.peek()).toBeUndefined();

	disposeOlder();
	await settle();
	expect(consumer.routes(path)).toBe(false);

	announced.close();
	origin.close();
});

test("withdrawing an answer wakes the requests table", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("handoff");

	const request = consumer.request(path);

	const first = new BroadcastProducer();
	const withdraw = origin.answer(path, first.consume());
	expect(withdraw).toBeDefined();

	// A second answer while one stands must lose and stay eligible.
	const second = new BroadcastProducer();
	expect(origin.answer(path, second.consume())).toBeUndefined();

	// Withdrawing pokes the requests map, which is what a standby serving loop sleeps on.
	const woken = origin.requests.changed();
	withdraw?.();
	await woken;
	expect(request.active.peek()).toBeUndefined();

	// The slot is vacant again, so a standby answers.
	const third = new BroadcastProducer();
	expect(origin.answer(path, third.consume())).toBeDefined();
	expect(request.active.peek()).toBeDefined();

	request.close();
	origin.close();
});

test("requests never appear in announced or the table", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("assumed");

	const request = consumer.request(path);
	const upstream = new BroadcastProducer();
	origin.answer(path, upstream.consume());

	// An answered request is assumed present, not known live, so it is not availability: it
	// stays out of the table, and out of the announcements the table drives.
	expect(request.active.peek()).toBeDefined();
	expect(origin.routes(path)).toBe(false);
	expect(consumer.broadcasts.peek()?.has(path)).toBe(false);

	const announced = consumer.announced();
	origin.publish(Path.from("real"));
	expect(await announced.next()).toEqual({ path: Path.from("real"), active: true });

	announced.close();
	request.close();
	origin.close();
});

test("a republish retracts then re-announces the path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	origin.publish(path);
	const announced = consumer.announced();
	expect(await announced.next()).toEqual({ path, active: true });

	// A new broadcast takes the path: consumers must let go of the superseded one.
	origin.publish(path);
	expect(await announced.next()).toEqual({ path, active: false });
	expect(await announced.next()).toEqual({ path, active: true });

	announced.close();
	origin.close();
});

test("the one-shot lookup is off the published surface", () => {
	const origin = new Producer();
	const consumer = origin.consume();

	// `get` was a snapshot that raced a republish, and `request` replaced it. Neither handle
	// may still carry it: an @internal tag would keep it in the emitted declarations, so the
	// method has to be gone rather than merely undocumented.
	expect("get" in consumer).toBe(false);
	expect("get" in origin).toBe(false);

	origin.close();
});

test("a routed path needs no blind answer", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("announced");

	// What a serving session scans: a request on a path the table routes resolves to that
	// route, so answering it blind would park a handle nothing reads.
	const upstream = new BroadcastProducer();
	const dispose = origin.insertRemote(path, upstream.consume());

	const request = consumer.request(path);
	expect(origin.routes(path)).toBe(true);
	expect(request.active.peek()).toBeDefined();

	// The route going away is what makes the request need an answer, so the serving loop has
	// to wake on the table, not just on the requests map.
	const woken = origin.changed();
	dispose();
	await woken;
	expect(origin.routes(path)).toBe(false);
	expect(request.active.peek()).toBeUndefined();

	request.close();
	upstream.close();
	origin.close();
});

test("discovery reflects the attached sessions", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	expect(consumer.discovery.peek()).toBeUndefined();

	const blind = origin.attach(false);
	expect(consumer.discovery.peek()).toBe(false);

	// Mixed: the table cannot be complete while one session announces nothing, so a consumer
	// gated on this has to keep asking rather than trusting the announcements.
	const seeing = origin.attach(true);
	expect(consumer.discovery.peek()).toBe(false);

	blind();
	expect(consumer.discovery.peek()).toBe(true);
	seeing();
	expect(consumer.discovery.peek()).toBeUndefined();

	origin.close();
});

test("the exposed getters are wirable as component inputs", () => {
	const origin = new Producer();
	const path = Path.from("wired");

	// getter() rejects a readable it did not create, so a hand-rolled object here would
	// throw the moment a consumer wired discovery or a request into a component.
	expect(() => getter(origin.discovery)).not.toThrow();

	const request = origin.request(path);
	expect(() => getter(request.active)).not.toThrow();

	request.close();
	origin.close();
});

test("closing what a request resolved leaves the path published for everyone else", async () => {
	const origin = new Producer();
	const path = Path.from("mine");

	const producer = origin.publish(path);
	const first = origin.request(path);
	const second = origin.request(path);

	const mine = first.active.peek();
	expect(mine).toBeDefined();
	// A handle of the request's own, not the table's front.
	expect(mine).not.toBe(second.active.peek());

	// The ordinary thing a caller does with a consumer they were handed. It must not reach
	// through to the table's handle and take the broadcast down with it.
	mine?.close();
	await settle();

	expect(producer.closed.peek()).toBeUndefined();
	expect(second.active.peek()?.closed.peek()).toBeUndefined();

	// A later request still resolves it too.
	const third = origin.request(path);
	expect(third.active.peek()).toBeDefined();
	expect(third.active.peek()?.closed.peek()).toBeUndefined();

	first.close();
	second.close();
	third.close();
	producer.close();
	origin.close();
});

test("closing a request releases the handle it was holding", async () => {
	const origin = new Producer();
	const path = Path.from("mine");

	const producer = origin.publish(path);
	const request = origin.request(path);
	expect(request.active.peek()).toBeDefined();

	request.close();
	await settle();

	// The handle is released and the view is gone, while the published broadcast, whose
	// handle belongs to the table, carries on.
	expect(request.active.peek()).toBeUndefined();
	expect(producer.closed.peek()).toBeUndefined();
	expect(origin.consume().routes(path)).toBe(true);

	producer.close();
	origin.close();
});

test("a retracted route is retired even for a request nobody reads again", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("redundant");

	const older = new BroadcastProducer();
	const newer = new BroadcastProducer();
	const disposeOlder = origin.insertRemote(path, older.consume());
	const disposeNewer = origin.insertRemote(path, newer.consume());

	const request = consumer.request(path);
	// One read, then the holder goes quiet: a peek-only holder must not pin the route.
	expect(request.active.peek()).toBeDefined();

	// The newest route retracts and the older one is promoted. Nothing reads `active`.
	disposeNewer();
	await settle();

	expect(newer.closed.peek()).not.toBeUndefined();

	request.close();
	disposeOlder();
	older.close();
	origin.close();
});

test("a request only wakes for its own path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const watched = Path.from("watched");
	const other = Path.from("other");

	const request = consumer.request(watched);
	let wakeups = 0;
	const dispose = request.active.subscribe(() => {
		wakeups += 1;
	});

	// Churn an unrelated path. Deriving each request over the whole table would wake this
	// one every time, which is what makes a busy origin cost O(requests) per publish.
	for (let i = 0; i < 5; i++) {
		const noise = origin.publish(other);
		await settle();
		noise.close();
		await settle();
	}
	expect(wakeups).toBe(0);

	// Its own path still reaches it.
	const mine = origin.publish(watched);
	await settle();
	expect(wakeups).toBe(1);
	expect(request.active.peek()).toBeDefined();

	dispose();
	request.close();
	mine.close();
	origin.close();
});

test("a request is unroutable only when nothing can answer it", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("nowhere");

	// Nothing published, nothing attached, nothing coming: waiting here is futile.
	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();
	expect(request.unroutable.peek()).toBe(true);

	// A session attaches: now the path is merely unanswered.
	const detach = origin.attach(false);
	await settle();
	expect(request.unroutable.peek()).toBe(false);

	// It goes back to unroutable when the session dies with nothing to replace it.
	detach();
	await settle();
	expect(request.unroutable.peek()).toBe(true);

	request.close();
	origin.close();
});

test("a reconnecting connection keeps requests pending across the gap", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("later");

	// What a reconnecting connection holds: no session yet, but one is coming. This is the
	// page-load window, and it must not read as a missing broadcast.
	const release = origin.expect();

	const request = consumer.request(path);
	expect(request.unroutable.peek()).toBe(false);

	// A session comes and goes; the expectation still covers the gap.
	const detach = origin.attach(true);
	await settle();
	detach();
	await settle();
	expect(request.unroutable.peek()).toBe(false);

	// Only giving up for good makes it unroutable.
	release();
	await settle();
	expect(request.unroutable.peek()).toBe(true);

	request.close();
	origin.close();
});

test("a routed path is never unroutable", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("here");

	const broadcast = origin.publish(path);
	const request = consumer.request(path);

	// Routed with nothing attached at all: the route is the answer, so no answerer is needed.
	expect(request.active.peek()).toBeDefined();
	expect(request.unroutable.peek()).toBe(false);

	// Unpublishing with nothing able to answer flips it.
	broadcast.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.unroutable.peek()).toBe(true);

	request.close();
	origin.close();
});

test("a request on a closed origin is unroutable", () => {
	const origin = new Producer();
	const consumer = origin.consume();
	origin.close();

	const request = consumer.request(Path.from("gone"));
	expect(request.active.peek()).toBeUndefined();
	expect(request.unroutable.peek()).toBe(true);
	request.close();
});

test("a seeded route still notifies when it retracts", async () => {
	const origin = new Producer();
	const path = Path.from("seeded");

	// Routed before the request exists, so the request is seeded rather than notified into
	// its first value. A silent seed leaves the pre-seed value as the baseline the next
	// change is compared against, which makes this retraction look like no change at all.
	const upstream = new BroadcastProducer();
	const dispose = origin.insertRemote(path, upstream.consume());

	const request = origin.consume().request(path);
	expect(request.active.peek()).toBeDefined();

	let wakeups = 0;
	const stop = request.active.subscribe(() => {
		wakeups += 1;
	});

	dispose();
	await settle();

	expect(wakeups).toBeGreaterThan(0);
	expect(request.active.peek()).toBeUndefined();

	stop();
	request.close();
	upstream.close();
	origin.close();
});

test("closing the origin makes an existing request unroutable", async () => {
	const origin = new Producer();
	const path = Path.from("doomed");

	const detach = origin.attach(true);
	const request = origin.consume().request(path);
	expect(request.unroutable.peek()).toBe(false);

	// The session is still attached, but a closed origin can never answer through it.
	origin.close();
	await settle();
	expect(request.unroutable.peek()).toBe(true);

	// The attached session releasing afterwards must not drive the count below zero and
	// resurrect the idea that something can answer.
	detach();
	await settle();
	expect(request.unroutable.peek()).toBe(true);

	request.close();
});

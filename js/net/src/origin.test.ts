import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

test("a published broadcast resolves by path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const path = Path.from("room");
	expect(consumer.get(path)).toBeUndefined();

	const broadcast = origin.publish(path);
	broadcast.createTrack("video");

	const handle = consumer.get(path);
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
	expect(consumer.get(path)).toBeDefined();

	broadcast.close();
	await settle();
	expect(consumer.get(path)).toBeUndefined();

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

	const handle = consumer.get(path);
	expect(handle).toBeDefined();
	handle?.close();

	second.close();
	await settle();
	expect(consumer.get(path)).toBeUndefined();

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
	const mine = consumer.get(path);
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

	expect(consumer.get(Path.from("a"))).toBeUndefined();
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

	const handle = consumer.get(path);
	expect(handle).toBeDefined();
	handle?.close();

	// Announced streams include remote entries.
	const announced = consumer.announced();
	expect(await announced.next()).toEqual({ path, active: true });

	dispose();
	expect(await announced.next()).toEqual({ path, active: false });
	expect(consumer.get(path)).toBeUndefined();

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
	const handle = consumer.get(path);
	const track = handle?.subscribe("local-track");
	expect(track).toBeDefined();
	track?.close();
	handle?.close();

	// One path, one announcement, even though both tables route it.
	const announced = consumer.announced();
	expect(await announced.next()).toEqual({ path, active: true });

	// Dropping the local publish falls back to the remote entry without a retraction.
	local.close();
	const back = consumer.get(path);
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
	slot?.front.set(upstream.consume());
	expect(request.active.peek()).toBeDefined();

	// A second request for the same path shares the answer.
	const again = consumer.request(path);
	expect(again.active.peek()).toBe(request.active.peek());
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

	expect(second.active.peek()).toBe(front);
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

	const handle = consumer.get(path);
	const track = handle?.subscribe("chat");
	expect(track).toBeDefined();
	track?.close();
	handle?.close();

	// The older source was never the origin's to close.
	expect(older.closed.peek()).toBeUndefined();

	disposeOlder();
	await settle();
	expect(consumer.get(path)).toBeUndefined();

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

test("requests never appear in announced or consume", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("assumed");

	const request = consumer.request(path);
	const upstream = new BroadcastProducer();
	origin.requests.peek()?.get(path)?.front.set(upstream.consume());

	// An answered request is assumed present, not known live, so it is not availability.
	expect(consumer.get(path)).toBeUndefined();
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

test("discovery reflects the attached sessions", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	expect(consumer.discovery.peek()).toBeUndefined();

	const blind = origin.attach(false);
	expect(consumer.discovery.peek()).toBe(false);

	const seeing = origin.attach(true);
	expect(consumer.discovery.peek()).toBe(true);

	seeing();
	expect(consumer.discovery.peek()).toBe(false);
	blind();
	expect(consumer.discovery.peek()).toBeUndefined();

	origin.close();
});

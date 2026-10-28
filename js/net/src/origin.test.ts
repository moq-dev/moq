import { expect, spyOn, test } from "bun:test";
import { getter, Once, race, Signal } from "@moq/signals";
import { announcedView } from "./announced.test-util";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import * as Epoch from "./epoch.ts";
import { StreamCode, StreamError } from "./error.ts";
import { HopSchema, Route } from "./hop.ts";
import { hooks, spreadHash } from "./internal.ts";
import { SessionBroadcast, textFrame } from "./mock.ts";
import type { Consumer, Table } from "./origin.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";
import { readsFrom } from "./resume.test-util";
import { Timescale } from "./time.ts";
import { overrideBroadcastWire, wireOf } from "./wire.ts";

function publish(origin: Producer, path: Path.Valid) {
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	return broadcast;
}

/** A peer's hop, for a route that is not local. */
const PEER = HopSchema.parse(10n);
// One publisher instance, so a local broadcast and a remote route compete on cost alone.
const EPOCH = Epoch.parse("01900000-0000-7000-8000-000000000001");

/** Land a received prefix, served from `consume`, the way a session does. */
function serve(origin: Producer, prefix: Path.Valid, consume: () => BroadcastConsumer, route: Route = Route.default) {
	const handle = wireOf(origin).receive(prefix, route);
	void (async () => {
		try {
			for await (const request of handle.requested()) {
				hooks.acceptRequest(request, consume());
			}
		} catch {
			handle.close();
		}
	})();
	return () => handle.close();
}

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
async function routed(consumer: Consumer, path: Path.Valid): Promise<BroadcastConsumer | undefined> {
	const request = consumer.request(path);
	await settle();
	const front = request.active.peek()?.clone();
	request.close();
	return front;
}

/** A stand-in for a session announcing a route: serves any path from `producer`. */
function provider(producer: BroadcastProducer) {
	return () => producer.consume();
}

test("a borrowed table exposes dynamic serving and a live scoped broadcast map", async () => {
	const origin = new Producer();
	const table: Table = origin;
	const scope = Path.Pattern.parse("room/**");
	const live = await announcedView(table, scope);
	const changes: ReadonlyMap<Path.Valid, Route>[] = [];
	const stop = live.subscribe((value) => changes.push(value));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().size).toBe(0);

	const other = publish(origin, Path.from("other"));
	const localPath = Path.from("room/alice");
	const localPublication = table.createBroadcast(localPath);
	const local = localPublication.broadcast;
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().has(localPath)).toBe(false);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().has(Path.from("other"))).toBe(false);

	localPublication.announce({ epoch: EPOCH, cost: 4n });
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(localPath)).toEqual(Route.normalize({ epoch: EPOCH, cost: 4n }));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(changes.some((value) => value.get(localPath)?.cost === 4n)).toBe(true);

	const prefix = Path.from("room");
	const dynamic = table.requestsRoute(prefix, { cost: 2n });
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(prefix)).toEqual(Route.normalize({ cost: 2n }));
	dynamic.updateRoute({ cost: 3n });
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(prefix)).toEqual(Route.normalize({ cost: 3n }));

	dynamic.close();
	local.close();
	other.close();
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().size).toBe(0);
	stop();
	origin.close();
});

test("an announced local path competes with a received route on cost", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room/alice");
	const remote = wireOf(origin).receive(path, { epoch: EPOCH, hops: [PEER], cost: 5n });
	const live = await announcedView(consumer);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(Route.normalize({ epoch: EPOCH, hops: [PEER], cost: 5n }));

	// Unannounced, it competes for nothing.
	const localPublication = origin.createBroadcast(path);
	const local = localPublication.broadcast;
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(Route.normalize({ epoch: EPOCH, hops: [PEER], cost: 5n }));

	// Cheaper wins; a tie falls to the local broadcast.
	localPublication.announce({ epoch: EPOCH, cost: 9n });
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(Route.normalize({ epoch: EPOCH, hops: [PEER], cost: 5n }));
	localPublication.announce({ epoch: EPOCH, cost: 5n });
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(Route.normalize({ epoch: EPOCH, cost: 5n }));

	local.close();
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(Route.normalize({ epoch: EPOCH, hops: [PEER], cost: 5n }));

	remote.close();
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().size).toBe(0);
	origin.close();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().size).toBe(0);
});

test("a scoped route remains visible when an exact local path is outside the scope", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const remote = wireOf(origin).receive(path, { cost: 5n });
	const local = publish(origin, path);
	const scope = Path.Pattern.parse("room/*");
	const live = await announcedView(origin, scope);

	await new Promise<void>((resolve) => setImmediate(resolve));

	expect((await announcedView(origin)).peek().get(path)).toMatchObject(Route.default);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(Route.normalize({ cost: 5n }));

	local.close();
	remote.close();
	origin.close();
});

test("a published broadcast resolves by path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const path = Path.from("room");
	expect(wireOf(consumer).routes(path)).toBe(false);

	const broadcast = publish(origin, path);
	broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const handle = await routed(consumer, path);
	expect(handle).toBeDefined();

	// The handle reaches the published tracks.
	const track = handle?.track("video").subscribe();
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

	const broadcast = publish(origin, path);
	expect(wireOf(consumer).routes(path)).toBe(true);

	broadcast.close();
	await settle();
	expect(wireOf(consumer).routes(path)).toBe(false);

	origin.close();
});

test("a stale broadcast closing does not unpublish a republished path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const first = publish(origin, path);
	const second = publish(origin, path);

	// The republish already superseded it, so this close must not remove the live one.
	first.close();
	await settle();

	const handle = await routed(consumer, path);
	expect(handle).toBeDefined();
	handle?.close();

	second.close();
	await settle();
	expect(wireOf(consumer).routes(path)).toBe(false);

	origin.close();
});

test("a republish closes the superseded broadcast", async () => {
	const origin = new Producer();
	const path = Path.from("room");

	const first = publish(origin, path);
	publish(origin, path);

	await settle();
	// The origin held the only handle on the first broadcast, so superseding it closed it.
	expect(first.closed.peek()).not.toBeUndefined();

	origin.close();
});

test("a superseded broadcast closes, and its subscribers keep reading", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const publication = origin.createBroadcast(path);
	const first = publication.broadcast;
	const track = first.createTrack("video", { timescale: Timescale.MILLI });
	publication.announce();
	const mine = await routed(consumer, path);
	const subscriber = mine?.track("video").subscribe();
	if (!subscriber) throw new Error("expected a subscriber");

	const replacement = origin.createBroadcast(path);
	replacement.announce();
	// The origin closes the old producer itself, whoever still holds a handle on it.
	expect(first.closed.peek()).toBeNull();
	// Its stale publication never displaces or retracts the replacement.
	expect(() => publication.announce({ epoch: EPOCH })).toThrow("broadcast is closed");
	publication.unannounce();
	const current = await routed(consumer, path);
	expect(readsFrom(current, replacement.broadcast)).toBe(true);
	current?.close();

	// A track already subscribed is not the broadcast: it keeps serving until it ends.
	track.writeFrame(textFrame("late"));
	expect((await subscriber.recvGroup())?.sequence).toBe(0);

	subscriber.close();
	mine?.close();
	track.finish();
	track.close();
	origin.close();
});

test("closing the origin closes every routed broadcast", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const a = publish(origin, Path.from("a"));
	const b = publish(origin, Path.from("b"));

	const abort = new Error("shutdown");
	origin.close(abort);

	expect(origin.closed.peek()).toBe(abort);
	expect(consumer.closed.peek()).toBe(abort);
	// A broadcast end carries no cause, so the routed broadcasts close cleanly.
	expect(a.closed.peek()).toBeNull();
	expect(b.closed.peek()).toBeNull();

	expect(wireOf(consumer).routes(Path.from("a"))).toBe(false);
	expect(() => publish(origin, Path.from("late"))).toThrow();

	// Idempotent: the first close wins.
	origin.close();
	expect(origin.closed.peek()).toBe(abort);
});

test("announced streams the table under a scope with origin-relative paths", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const a = publish(origin, Path.from("room/a"));

	const announced = consumer.announced(Path.Pattern.subtree(Path.from("room")));

	// The initial state arrives first, named from the origin rather than the scope.
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room/a"), kind: "start" });

	// Additions under the scope stream in; paths outside it are invisible.
	const b = publish(origin, Path.from("room/b"));
	publish(origin, Path.from("lobby/c"));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room/b"), kind: "start" });

	// Removals retract.
	b.close();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room/b"), kind: "end" });

	// The stream ends when the origin closes.
	origin.close();
	expect(await announced.next()).toBeUndefined();

	a.close();
});

test("hidden paths need an opt-in or a scope naming the dot segment", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const hidden = publish(origin, Path.from(".stats/node"));
	const nested = publish(origin, Path.from("room/.internal"));
	const visible = publish(origin, Path.from("room/catalog.pro"));

	await new Promise<void>((resolve) => setImmediate(resolve));

	expect([...(await announcedView(consumer)).peek().keys()]).toEqual([Path.from("room/catalog.pro")]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announcedView(consumer, undefined, { hidden: true })).peek().size).toBe(3);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(await announcedView(consumer, Path.Pattern.parse(".stats/**"))).peek().keys()]).toEqual([
		Path.from(".stats/node"),
	]);

	const plain = consumer.announced();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(await plain.next()).toMatchObject({ prefix: Path.from("room/catalog.pro") });
	const opted = consumer.announced(Path.Pattern.parse("room/**"), { hidden: true });
	const seen = [(await opted.next())?.prefix, (await opted.next())?.prefix].sort();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(seen).toEqual([Path.from("room/.internal"), Path.from("room/catalog.pro")]);

	plain.close();
	opted.close();
	hidden.close();
	nested.close();
	visible.close();
	origin.close();
});

test("a remote entry resolves by path and retracts on dispose", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("relayed");

	// Stand in for a session's discovered broadcast.
	const upstream = new BroadcastProducer();
	const dispose = serve(origin, path, provider(upstream));

	const handle = await routed(consumer, path);
	expect(handle).toBeDefined();
	handle?.close();

	// Announced streams include remote entries.
	const announced = consumer.announced();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start", route: Route.default });

	dispose();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "end" });
	expect(wireOf(consumer).routes(path)).toBe(false);

	announced.close();
	upstream.close();
	origin.close();
});

test("announced keeps a broader covering route at its prefix", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const upstream = new BroadcastProducer();
	const dispose = serve(origin, Path.from("room"), provider(upstream));

	// The scope filters the route without changing the prefix it claims.
	const scope = Path.Pattern.subtree(Path.from("room/alice"));
	const announced = consumer.announced(scope);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	dispose();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "end" });

	announced.close();
	upstream.close();
	origin.close();
});

test("a local publish shadows a remote entry", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const upstream = new BroadcastProducer();
	upstream.createTrack("remote-track", { timescale: Timescale.MILLI });
	const dispose = serve(origin, path, provider(upstream), { hops: [], cost: 9n });

	const local = publish(origin, path);
	local.createTrack("local-track", { timescale: Timescale.MILLI });

	// Local wins: the handle reaches the local track, not the remote one.
	const handle = await routed(consumer, path);
	const track = handle?.track("local-track").subscribe();
	expect(track).toBeDefined();
	track?.close();
	handle?.close();

	// One path, one announcement, even though both tables route it.
	const announced = consumer.announced();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start", route: Route.default });

	// Dropping the local publish falls back to the remote entry without a retraction.
	local.close();
	const back = await routed(consumer, path);
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

	publish(origin, Path.from("mine"));
	const upstream = new BroadcastProducer();
	serve(origin, Path.from("theirs"), provider(upstream));

	// What a session announces to a peer: local only, so a shared origin cannot echo.
	const table = wireOf(consumer).advertised.peek();
	expect(table?.has(Path.from("mine"))).toBe(true);
	expect(table?.has(Path.from("theirs"))).toBe(false);

	upstream.close();
	origin.close();
});

test("inserting into a closed origin routes nothing", async () => {
	const origin = new Producer();
	origin.close();

	const upstream = new BroadcastProducer();
	const dispose = serve(origin, Path.from("late"), provider(upstream));
	dispose();

	// Nothing was materialized, so the provider's broadcast is untouched.
	await settle();
	expect(upstream.closed.peek()).toBeUndefined();
	upstream.close();
});

test("a request resolves once a front answers, and survives its withdrawal", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("wanted");

	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();

	// A session answers (simulated): the slot's front resolves the request.
	const upstream = new SessionBroadcast();
	const slot = wireOf(origin).requests.peek()?.get(path);
	expect(slot).toBeDefined();
	expect(wireOf(origin).answer(path, upstream)).toBeDefined();
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
	expect(wireOf(origin).requests.peek()?.has(path)).toBe(false);
	expect(upstream.closed.peek()).not.toBeUndefined();

	origin.close();
});

test("a request closed and retaken in the same tick keeps its answer", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("stable");

	const first = consumer.request(path);
	const upstream = new BroadcastProducer();
	const withdraw = wireOf(origin).answer(path, upstream.consume());
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
	older.createTrack("chat", { timescale: Timescale.MILLI });
	const keepOlder = older.consume();
	const disposeOlder = serve(origin, path, provider(older));

	const newer = new BroadcastProducer();
	const keepNewer = newer.consume();
	const disposeNewer = serve(origin, path, provider(newer));

	const announced = consumer.announced();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });

	// The newer session dies: without an epoch the promoted fallback is another source.
	disposeNewer();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart" });

	const handle = await routed(consumer, path);
	const track = handle?.track("chat").subscribe();
	expect(track).toBeDefined();
	track?.close();
	handle?.close();

	// The older source was never the origin's to close.
	expect(older.closed.peek()).toBeUndefined();

	disposeOlder();
	await settle();
	expect(wireOf(consumer).routes(path)).toBe(false);

	keepOlder.close();
	keepNewer.close();
	announced.close();
	origin.close();
});

test("withdrawing an answer wakes the requests table", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("handoff");

	const request = consumer.request(path);

	const first = new BroadcastProducer();
	const withdraw = wireOf(origin).answer(path, first.consume());
	expect(withdraw).toBeDefined();

	// A second answer while one stands must lose and stay eligible.
	const second = new BroadcastProducer();
	expect(wireOf(origin).answer(path, second.consume())).toBeUndefined();

	// Withdrawing pokes the requests map, which is what a standby serving loop sleeps on. The
	// request it answered ends: the next answer is another session, so another instance.
	const woken = wireOf(origin).requests.changed();
	withdraw?.();
	await woken;
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeInstanceOf(StreamError);

	// A fresh request is vacant again, so a standby answers it.
	const fresh = consumer.request(path);
	const third = new BroadcastProducer();
	expect(wireOf(origin).answer(path, third.consume())).toBeDefined();
	expect(fresh.active.peek()).toBeDefined();

	fresh.close();
	request.close();
	origin.close();
});

test("requests never appear in announced or the table", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("assumed");

	const request = consumer.request(path);
	const upstream = new BroadcastProducer();
	wireOf(origin).answer(path, upstream.consume());

	// An answered request is assumed present, not known live, so it is not availability: it
	// stays out of the table, and out of the announcements the table drives.
	expect(request.active.peek()).toBeDefined();
	expect(wireOf(origin).routes(path)).toBe(false);

	const announced = consumer.announced();
	publish(origin, Path.from("real"));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("real"), kind: "start" });

	announced.close();
	request.close();
	origin.close();
});

test("a republish restarts the path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	publish(origin, path);
	const announced = consumer.announced();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });

	// A new broadcast takes the path: consumers must let go of the replaced one.
	publish(origin, path);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart" });

	announced.close();
	origin.close();
});

test("an identical reconnect restarts without an epoch and is quiet with one", async () => {
	for (const epoch of [undefined, EPOCH]) {
		const origin = new Producer();
		const consumer = origin.consume();
		const path = Path.from("room");
		const announced = consumer.announced();
		const route = Route.normalize({ epoch, hops: [PEER] });

		const old = wireOf(origin).receive(path, route);
		expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });

		// The fresh session wins the tie: another source unless the epoch says otherwise.
		const fresh = wireOf(origin).receive(path, route);
		if (epoch === undefined) expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart" });

		// Retracting the stale twin is quiet, and a re-price of the winner is an update.
		old.close();
		fresh.updateRoute({ ...route, cost: 5n });
		expect(await announced.next()).toMatchObject({ prefix: path, kind: "update", route: { cost: 5n } });

		fresh.close();
		announced.close();
		origin.close();
	}
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
	const dispose = serve(origin, path, provider(upstream));

	const request = consumer.request(path);
	await settle();
	expect(wireOf(origin).routes(path)).toBe(true);
	expect(request.active.peek()).toBeDefined();

	// The route going away is what makes the request need an answer, so the serving loop has
	// to wake on the table, not just on the requests map.
	const woken = race([wireOf(origin).changed()]);
	dispose();
	await woken;
	expect(wireOf(origin).routes(path)).toBe(false);
	expect(request.active.peek()).toBeUndefined();

	request.close();
	upstream.close();
	origin.close();
});

test("discovery reflects the attached sessions", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	expect(consumer.discovery.peek()).toBeUndefined();

	const blind = wireOf(origin).attach(false);
	expect(consumer.discovery.peek()).toBe(false);

	// Mixed: the table cannot be complete while one session announces nothing, so a consumer
	// gated on this has to keep asking rather than trusting the announcements.
	const seeing = wireOf(origin).attach(true);
	expect(consumer.discovery.peek()).toBe(false);

	blind();
	expect(consumer.discovery.peek()).toBe(true);
	seeing();
	expect(consumer.discovery.peek()).toBeUndefined();

	origin.close();
});

test("announced requests share no subscription and follow discovery", () => {
	const origin = new Producer();
	const wire = wireOf(origin);

	const subscribers = new Map<Signal<unknown>, number>();
	const subscribe = Signal.prototype.subscribe;
	const spy = spyOn(Signal.prototype, "subscribe").mockImplementation(function (
		this: Signal<unknown>,
		fn: (value: unknown) => void,
	) {
		subscribers.set(this, (subscribers.get(this) ?? 0) + 1);
		return subscribe.call(this, fn);
	});

	const requests = [];
	try {
		for (let i = 0; i < 200; i++) requests.push(origin.request(Path.from(`user${i}`), { announced: true }));
	} finally {
		spy.mockRestore();
	}

	// Each path has its own retirement and active-handle observers, never a shared table observer.
	expect(Math.max(...subscribers.values())).toBe(2);

	const slots = [...(wire.requests.peek()?.values() ?? [])];
	expect(slots.length).toBe(200);

	// Following remains gated even when a session cannot announce.
	expect(slots.some((slot) => wire.blind(slot))).toBe(false);
	const seeing = wire.attach(true);
	expect(slots.some((slot) => wire.blind(slot))).toBe(false);
	const silent = wire.attach(false);
	expect(slots.some((slot) => wire.blind(slot))).toBe(false);
	silent();
	expect(slots.some((slot) => wire.blind(slot))).toBe(false);

	for (const request of requests) request.close();
	seeing();
	origin.close();
});

test("the exposed getters are wirable as component inputs", () => {
	const origin = new Producer();
	const path = Path.from("wired");

	// Derived and other package Getters are wirable as component inputs.
	expect(() => getter(origin.discovery)).not.toThrow();

	const request = origin.request(path);
	expect(() => getter(request.active)).not.toThrow();

	request.close();
	origin.close();
});

test("closing what a request resolved leaves the path published for everyone else", async () => {
	const origin = new Producer();
	const path = Path.from("mine");

	const producer = publish(origin, path);
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

	const producer = publish(origin, path);
	const request = origin.request(path);
	expect(request.active.peek()).toBeDefined();

	request.close();
	await settle();

	// The handle is released and the view is gone, while the published broadcast, whose
	// handle belongs to the table, carries on.
	expect(request.active.peek()).toBeUndefined();
	expect(producer.closed.peek()).toBeUndefined();
	expect(wireOf(origin.consume()).routes(path)).toBe(true);

	producer.close();
	origin.close();
});

test("a retracted route is retired even for a request nobody reads again", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("redundant");

	const older = new BroadcastProducer();
	const newer = new BroadcastProducer();
	// Model a session provider faithfully: the session holds its own handle on the
	// broadcast for its lifetime and lends out clones, so releasing a materialized
	// front never closes the producer.
	const keepOlder = older.consume();
	const keepNewer = newer.consume();
	const disposeOlder = serve(origin, path, () => keepOlder.clone());
	const disposeNewer = serve(origin, path, () => keepNewer.clone());

	const request = consumer.request(path);
	await settle();
	// One read, then the holder goes quiet: a peek-only holder must not pin the route.
	expect(request.active.peek()).toBeDefined();

	// The newest route retracts and the older one is promoted. Nothing reads
	// `active`, but the retracted route's materialized subscription is released
	// all the same (the producer itself belongs to the session, so it stays open).
	disposeNewer();
	await settle();

	expect(newer.closed.peek()).toBeUndefined();

	request.close();
	disposeOlder();
	keepNewer.close();
	keepOlder.close();
	newer.close();
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
		const noise = publish(origin, other);
		await settle();
		noise.close();
		await settle();
	}
	expect(wakeups).toBe(0);

	// Its own path still reaches it.
	const mine = publish(origin, watched);
	await settle();
	expect(wakeups).toBe(1);
	expect(request.active.peek()).toBeDefined();

	dispose();
	request.close();
	mine.close();
	origin.close();
});

test("a request on a closed origin ends at once", () => {
	const origin = new Producer();
	const consumer = origin.consume();
	origin.close();

	const request = consumer.request(Path.from("gone"));
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeNull();
	request.close();
});

test("a seeded route still notifies when it retracts", async () => {
	const origin = new Producer();
	const path = Path.from("seeded");

	// Routed before the request exists, so the request is seeded rather than notified into
	// its first value. A silent seed leaves the pre-seed value as the baseline the next
	// change is compared against, which makes this retraction look like no change at all.
	const upstream = new BroadcastProducer();
	const dispose = serve(origin, path, provider(upstream));

	const request = origin.consume().request(path);
	await settle();
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

test("createBroadcast is invisible to everyone until announce", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	expect(wireOf(consumer).routes(path)).toBe(false);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(false);
	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();

	const announced = consumer.announced();
	broadcastPublication.announce({ cost: 4n });
	expect(wireOf(consumer).routes(path)).toBe(true);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(true);
	expect(await announced.next()).toMatchObject({
		prefix: path,
		kind: "start",
		route: { hops: [], cost: 4n },
	});
	const first = request.active.peek();
	expect(first).toBeDefined();

	// Off the air for local consumers and peers alike, which ends the request on it.
	broadcastPublication.unannounce();
	expect(wireOf(consumer).routes(path)).toBe(false);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(false);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "end" });
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeInstanceOf(Error);

	// Back on the air for a fresh request, through a fresh handle.
	broadcastPublication.announce();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });
	const fresh = consumer.request(path);
	const again = fresh.active.peek();
	expect(again).toBeDefined();
	expect(again).not.toBe(first);

	broadcast.close();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "end" });
	fresh.close();
	request.close();
	announced.close();
	origin.close();
});

test("a request prefers a cheaper received route over an announced local broadcast", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const upstream = new BroadcastProducer();
	const dispose = serve(origin, path, provider(upstream), Route.normalize({ epoch: EPOCH, hops: [PEER], cost: 1n }));
	const localPublication = origin.createBroadcast(path);
	const local = localPublication.broadcast;
	localPublication.announce({ epoch: EPOCH, cost: 3n });

	// The cheaper route is served on demand, so it resolves only once its handler answers.
	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();
	await settle();
	const remote = request.active.peek();
	expect(remote).toBeDefined();
	const announced = consumer.announced();
	expect(await announced.next()).toMatchObject({ prefix: path, route: { hops: [PEER] } });

	// Re-priced below it, the local broadcast of the same instance wins at once: the request's
	// broadcast stays, and its tracks read the local one.
	localPublication.announce({ epoch: EPOCH, cost: 0n });
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "update", route: { epoch: EPOCH, hops: [] } });
	expect(request.active.peek()).toBe(remote);
	expect(readsFrom(remote, local)).toBe(true);

	announced.close();
	request.close();
	local.close();
	dispose();
	upstream.close();
	origin.close();
});

test("the cheapest received route competes with the local broadcast, not the newest", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const upstream = new BroadcastProducer();
	const served: string[] = [];
	const via = (name: string) => () => {
		served.push(name);
		return upstream.consume();
	};
	const cheap = serve(origin, path, via("cheap"), Route.normalize({ epoch: EPOCH, hops: [PEER], cost: 1n }));
	const pricey = serve(origin, path, via("pricey"), Route.normalize({ epoch: EPOCH, hops: [PEER], cost: 10n }));
	const localPublication = origin.createBroadcast(path);
	const local = localPublication.broadcast;
	localPublication.announce({ epoch: EPOCH, cost: 5n });

	await new Promise<void>((resolve) => setImmediate(resolve));

	expect((await announcedView(consumer)).peek().get(path)).toEqual(
		Route.normalize({ epoch: EPOCH, hops: [PEER], cost: 1n }),
	);
	const request = consumer.request(path);
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(request.active.peek()).toBeDefined();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(served).toEqual(["cheap"]);

	request.close();
	local.close();
	pricey();
	cheap();
	upstream.close();
	origin.close();
});

test("a handle serves a request under live", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const handle = origin.requestsRoute(Path.from("live"));

	expect(await consumer.announced().next()).toMatchObject({ prefix: Path.from("live"), kind: "start" });

	const waiting = handle.requested().next();
	const request = consumer.request(Path.from("live/cam"));
	const { value: req } = await waiting;
	expect(req?.path).toBe(Path.from("live/cam"));

	const produced = new BroadcastProducer();
	produced.createTrack("video", { timescale: Timescale.MILLI });
	hooks.acceptRequest(req, produced);
	await settle();

	expect(request.active.peek()).toBeDefined();
	const track = request.active.peek()?.track("video").subscribe();
	expect(track).toBeDefined();
	track?.close();

	request.close();
	handle.close();
	produced.close();
	origin.close();
});

test("an originated route refuses a broadcast a session delivered", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const it = handle.requested();
	const pending = wireOf(origin.consume()).demand(Path.from("live/cam"));
	const { value: req } = await it.next();
	if (!req) throw new Error("expected request");

	// What lite and IETF do when a session hands back a consumed broadcast.
	const foreign = new BroadcastProducer().consume();
	overrideBroadcastWire(foreign, {});
	expect(() => hooks.acceptRequest(req, foreign)).toThrow("origin cannot serve a broadcast it did not produce");

	const produced = new BroadcastProducer();
	produced.createTrack("video");
	hooks.acceptRequest(req, produced.consume());
	expect(await pending).toBeDefined();

	handle.close();
	produced.close();
	foreign.close();
	origin.close();
});

test("a received route still serves a broadcast a session delivered", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const upstream = new BroadcastProducer();
	upstream.createTrack("video");
	const foreign = upstream.consume();
	overrideBroadcastWire(foreign, {});
	const stop = serve(origin, path, () => foreign);

	const request = origin.request(path);
	await settle();
	expect(request.active.peek()?.track("video").subscribe()).toBeDefined();

	request.close();
	stop();
	upstream.close();
	origin.close();
});

test("a re-priced route is delivered as an update", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const announced = origin.consume().announced();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("live"), kind: "start" });
	handle.updateRoute({ cost: 5n });
	expect(await announced.next()).toMatchObject({ prefix: Path.from("live"), kind: "update" });
	handle.close();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("live"), kind: "end" });
	announced.close();
	origin.close();
});

test("an accepted dynamic broadcast is retired when it closes", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const path = Path.from("live/cam");
	const request = origin.request(path);
	const it = handle.requested();

	const first = await it.next();
	const produced = new BroadcastProducer();
	produced.createTrack("video", { timescale: Timescale.MILLI });
	hooks.acceptRequest(first.value, produced);
	await settle();
	expect(request.active.peek()?.track("video").subscribe()).toBeDefined();

	produced.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();

	const second = await it.next();
	expect(second.value?.path).toBe(path);
	const replacement = new BroadcastProducer();
	replacement.createTrack("video", { timescale: Timescale.MILLI });
	hooks.acceptRequest(second.value, replacement);
	await settle();
	expect(request.active.peek()?.track("video").subscribe()).toBeDefined();

	request.close();
	handle.close();
	replacement.close();
	origin.close();
});

test("reject surfaces the error from demand", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const consumer = origin.consume();
	const it = handle.requested();
	const pending = wireOf(consumer).demand(Path.from("live/cam"));
	const { value: req } = await it.next();
	const err = new StreamError(StreamCode.NotFound, { message: "not here" });
	req?.reject(err);
	await expect(pending).rejects.toBe(err);

	handle.close();
	origin.close();
});

test("a peer is offered and served the cheapest originated route", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const prefix = Path.from("live");
	const cheap = origin.requestsRoute(prefix, { epoch: EPOCH, cost: 1n });
	const pricey = origin.requestsRoute(prefix, { epoch: EPOCH, cost: 10n });

	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(
		Route.normalize({ epoch: EPOCH, cost: 1n }),
	);
	const pending = wireOf(consumer).demand(Path.from("live/cam"));
	const { value: req } = await cheap.requested().next();
	const upstream = new BroadcastProducer();
	hooks.acceptRequest(req, upstream.consume());
	expect(await pending).toBeDefined();

	// An exact-path local broadcast competes with them on cost too.
	const localPublication = origin.createBroadcast(prefix);
	const local = localPublication.broadcast;
	localPublication.announce({ epoch: EPOCH, cost: 5n });
	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(
		Route.normalize({ epoch: EPOCH, cost: 1n }),
	);
	localPublication.announce({ epoch: EPOCH, cost: 0n });
	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(
		Route.normalize({ epoch: EPOCH, cost: 0n }),
	);

	local.close();
	upstream.close();
	pricey.close();
	cheap.close();
	origin.close();
});

test("a shared reject still surfaces from demand", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const consumer = origin.consume();
	const it = handle.requested();

	const request = origin.request(Path.from("live/cam"));
	const pending = wireOf(consumer).demand(Path.from("live/cam"));
	const { value: req } = await it.next();
	const err = new Error("unserved");
	req?.reject(err);
	await expect(pending).rejects.toBe(err);
	await settle();
	expect(request.active.peek()).toBeUndefined();
	const next = it.next();
	expect(await Promise.race([next.then(() => "queued"), settle().then(() => "idle")])).toBe("idle");

	request.close();
	handle.close();
	origin.close();
});

test("a request refusal does not keep demand from asking again", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const consumer = origin.consume();
	const it = handle.requested();

	const request = origin.request(Path.from("live/cam"));
	const { value: first } = await it.next();
	first?.reject(new Error("unserved"));
	request.close();
	await settle();

	const pending = wireOf(consumer).demand(Path.from("live/cam"));
	const { value: second } = await it.next();
	expect(second?.path).toBe(Path.from("live/cam"));
	const produced = new BroadcastProducer();
	hooks.acceptRequest(second, produced);
	await expect(pending).resolves.toBeDefined();

	handle.close();
	produced.close();
	origin.close();
});

test("advancing requested without settling rejects the previous request", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const consumer = origin.consume();
	const it = handle.requested();

	const firstDemand = wireOf(consumer).demand(Path.from("live/cam"));
	const first = await it.next();
	expect(first.value?.path).toBe(Path.from("live/cam"));

	const secondDemand = wireOf(consumer).demand(Path.from("live/other"));
	const second = await it.next();
	expect(second.value?.path).toBe(Path.from("live/other"));

	await expect(firstDemand).rejects.toMatchObject({ code: StreamCode.Unroutable });
	const produced = new BroadcastProducer();
	hooks.acceptRequest(second.value, produced);
	await expect(secondDemand).resolves.toBeDefined();

	handle.close();
	produced.close();
	origin.close();
});

test("a rejected request is not asked of the same route again", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const it = handle.requested();

	const request = origin.request(Path.from("live/cam"));
	const { value: first } = await it.next();
	expect(first?.path).toBe(Path.from("live/cam"));
	first?.reject(new Error("unserved"));
	await settle();

	// A refusal is authoritative: the path resolves to nothing rather than landing back on
	// the queue, so the handler is asked once.
	expect(request.active.peek()).toBeUndefined();
	const next = it.next();
	expect(await Promise.race([next.then(() => "queued"), settle().then(() => "idle")])).toBe("idle");

	// A fresh request asks again: the refusal was the request's own, not the path's.
	request.close();
	await settle();
	const retry = origin.request(Path.from("live/cam"));
	expect((await next).value?.path).toBe(Path.from("live/cam"));

	retry.close();
	handle.close();
	origin.close();
});

// A relay migration lands the replacement session's route next to the draining one's. Under
// one epoch the request must hand over to it, not drop to nothing while the new session answers.
test("an outranked route keeps serving until its replacement answers", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("migrating");

	const older = new BroadcastProducer();
	const keepOlder = older.consume();
	const disposeOlder = serve(origin, path, () => keepOlder.clone(), Route.normalize({ epoch: EPOCH }));

	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	let gaps = 0;
	const stop = request.active.subscribe((active) => {
		if (active === undefined) gaps++;
	});

	// The newer route wins the tie but has not answered yet.
	const newer = wireOf(origin).receive(path, { epoch: EPOCH });
	const asked = newer.requested().next();
	await settle();
	expect(request.active.peek()).toBe(first);

	// Once it answers, the request's tracks read it, under the same broadcast.
	const replacement = new BroadcastProducer();
	const { value: req } = await asked;
	hooks.acceptRequest(req, replacement.consume());
	await settle();
	expect(request.active.peek()).toBe(first);
	expect(readsFrom(first, replacement)).toBe(true);
	expect(gaps).toBe(0);

	stop();
	request.close();
	newer.close();
	disposeOlder();
	keepOlder.close();
	older.close();
	replacement.close();
	origin.close();
});

// Without an epoch nothing says the newer route serves the same bytes: the request already open
// stays on its source, while a new one resolves the newer route on a fresh front.
test("a request stays on its source when another instance wins", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("restarted");

	const older = new BroadcastProducer();
	const keepOlder = older.consume();
	const disposeOlder = serve(origin, path, () => keepOlder.clone());

	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	const newer = new BroadcastProducer();
	const disposeNewer = serve(origin, path, provider(newer));
	await settle();
	expect(request.active.peek()).toBe(first);
	expect(first?.closed.peek()).toBeUndefined();

	const fresh = consumer.request(path);
	await settle();
	expect(fresh.active.peek()).toBeDefined();
	expect(fresh.active.peek()).not.toBe(first);

	request.close();
	fresh.close();
	disposeNewer();
	disposeOlder();
	keepOlder.close();
	older.close();
	newer.close();
	origin.close();
});

// A request left on a replaced instance still hears that instance go: it ends with an error
// rather than moving onto the winner, which may not hold the same bytes.
test("a request on a replaced instance ends once its source goes", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("restarted");

	const older = new BroadcastProducer();
	const keepOlder = older.consume();
	const disposeOlder = serve(origin, path, () => keepOlder.clone());
	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	const newer = new BroadcastProducer();
	const disposeNewer = serve(origin, path, provider(newer));
	await settle();
	expect(request.active.peek()).toBe(first);

	disposeOlder();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeInstanceOf(StreamError);

	request.close();
	disposeNewer();
	keepOlder.close();
	older.close();
	newer.close();
	origin.close();
});

// Without an epoch nothing says a standby serves the winner's bytes: the winner going ends the
// request instead of handing it to the standby.
// A request left on a replaced instance still fails over within that instance: another route
// of its epoch serves the same bytes, so the request moves there rather than ending, while
// fresh requests resolve the newer epoch.
test("a request on a replaced epoch fails over to another route of it", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("replicated");
	const newer = Epoch.parse("01900000-0000-7000-8000-000000000002");

	const primary = new BroadcastProducer();
	const keepPrimary = primary.consume();
	const disposePrimary = serve(origin, path, () => keepPrimary.clone(), Route.normalize({ epoch: EPOCH }));
	const replica = new BroadcastProducer();
	const disposeReplica = serve(origin, path, provider(replica), Route.normalize({ epoch: EPOCH, cost: 5n }));
	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	const restarted = new BroadcastProducer();
	const disposeRestarted = serve(origin, path, provider(restarted), Route.normalize({ epoch: newer }));
	await settle();
	expect(request.active.peek()).toBe(first);

	disposePrimary();
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	expect(request.active.peek()).toBe(first);
	expect(first?.epoch).toBe(EPOCH);
	expect(readsFrom(first, replica)).toBe(true);

	const fresh = consumer.request(path);
	await settle();
	expect(fresh.active.peek()?.epoch).toBe(newer);

	// The replica going too ends it: nothing else serves its epoch.
	disposeReplica();
	await settle();
	expect(request.closed.peek()).toBeInstanceOf(StreamError);

	fresh.close();
	request.close();
	disposeRestarted();
	keepPrimary.close();
	primary.close();
	replica.close();
	restarted.close();
	origin.close();
});

// A local broadcast announced under the held epoch is a replica too.
test("a request on a replaced epoch fails over to a local replica of it", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("replicated");
	const newer = Epoch.parse("01900000-0000-7000-8000-000000000002");

	const primary = new BroadcastProducer();
	const keepPrimary = primary.consume();
	const disposePrimary = serve(origin, path, () => keepPrimary.clone(), Route.normalize({ epoch: EPOCH }));
	const localPublication = origin.createBroadcast(path);
	const local = localPublication.broadcast;
	localPublication.announce({ epoch: EPOCH, cost: 5n });
	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	const restarted = new BroadcastProducer();
	const disposeRestarted = serve(origin, path, provider(restarted), Route.normalize({ epoch: newer }));
	await settle();
	expect(request.active.peek()).toBe(first);

	disposePrimary();
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	expect(request.active.peek()).toBe(first);
	expect(first?.epoch).toBe(EPOCH);
	expect(readsFrom(first, local)).toBe(true);

	const fresh = consumer.request(path);
	await settle();
	expect(fresh.active.peek()?.epoch).toBe(newer);

	fresh.close();
	request.close();
	disposeRestarted();
	local.close();
	keepPrimary.close();
	primary.close();
	restarted.close();
	origin.close();
});

// A takeover asked while the original source was briefly gone speaks for nobody once it is back:
// its late refusal leaves the request on that source.
test("a stale takeover's refusal leaves a request whose source came back", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("resumed");
	const newer = Epoch.parse("01900000-0000-7000-8000-000000000002");

	const localPublication = origin.createBroadcast(path);
	const local = localPublication.broadcast;
	localPublication.announce({ epoch: EPOCH });
	const standby = wireOf(origin).receive(path, Route.normalize({ epoch: EPOCH, cost: 5n }));
	const asked = standby.requested();
	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	const restarted = new BroadcastProducer();
	const disposeRestarted = serve(origin, path, provider(restarted), Route.normalize({ epoch: newer }));
	await settle();
	expect(request.active.peek()).toBe(first);

	// The local broadcast steps away, so the standby is asked to take over; it comes back first.
	localPublication.unannounce();
	const takeover = await asked.next();
	localPublication.announce({ epoch: EPOCH });
	await settle();
	takeover.value?.reject(new Error("too late"));
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	expect(request.active.peek()).toBe(first);

	request.close();
	disposeRestarted();
	standby.close();
	local.close();
	restarted.close();
	origin.close();
});

test("a request ends when its winning source goes, even with a standby", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("standby");

	const standby = new BroadcastProducer();
	const disposeStandby = serve(origin, path, provider(standby), Route.normalize({ cost: 5n }));
	const winner = new BroadcastProducer();
	const keepWinner = winner.consume();
	const disposeWinner = serve(origin, path, () => keepWinner.clone());
	const request = consumer.request(path);
	await settle();
	expect(request.active.peek()).toBeDefined();

	disposeWinner();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeInstanceOf(StreamError);

	// A fresh request resolves the standby.
	const fresh = consumer.request(path);
	await settle();
	expect(fresh.active.peek()).toBeDefined();

	fresh.close();
	request.close();
	disposeStandby();
	keepWinner.close();
	winner.close();
	standby.close();
	origin.close();
});

test("a refusal is terminal, never falling through to a broader route", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const narrow = origin.requestsRoute(Path.from("live"));
	const wide = origin.requestsRoute(Path.from(""));
	const wideRequests = wide.requested();
	const wideAsked = wideRequests.next();

	const request = consumer.request(Path.from("live/cam"));
	const { value: req } = await narrow.requested().next();
	const err = new Error("unserved");
	req?.reject(err);
	await settle();

	// The narrow route spoke for the path, so the broader one is never asked.
	expect(await request.closed).toBe(err);
	expect(request.active.peek()).toBeUndefined();
	expect(await Promise.race([wideAsked.then(() => "asked"), settle().then(() => "idle")])).toBe("idle");

	request.close();
	void wideRequests.return?.();
	narrow.close();
	wide.close();
	origin.close();
});

test("a better route's refusal ends a served request", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const served = new BroadcastProducer();
	// One epoch, so the narrower route serves the same instance and the request follows it.
	const disposeWide = serve(origin, Path.from(""), provider(served), Route.normalize({ epoch: EPOCH }));

	const request = consumer.request(Path.from("live/cam"));
	await settle();
	const before = request.active.peek();
	expect(before).toBeDefined();

	// The narrower route is asked while the broad one keeps serving, then says no. A
	// costlier sibling at the same prefix could serve, but is never asked.
	const narrow = origin.requestsRoute(Path.from("live"), { epoch: EPOCH, cost: 1n });
	const sibling = origin.requestsRoute(Path.from("live"), { epoch: EPOCH, cost: 2n });
	const siblingRequests = sibling.requested();
	const siblingAsked = siblingRequests.next();
	const { value: req } = await narrow.requested().next();
	expect(request.active.peek()).toBe(before);
	const err = new Error("unserved");
	req?.reject(err);
	await settle();

	expect(await request.closed).toBe(err);
	expect(request.active.peek()).toBeUndefined();
	expect(await Promise.race([siblingAsked.then(() => "asked"), settle().then(() => "idle")])).toBe("idle");

	request.close();
	void siblingRequests.return?.();
	sibling.close();
	narrow.close();
	disposeWide();
	served.close();
	origin.close();
});

test("a refusal from a superseded route does not end the request", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const wide = origin.requestsRoute(Path.from(""));
	const wideRequests = wide.requested();

	const request = consumer.request(Path.from("live/cam"));
	const { value: stale } = await wideRequests.next();

	// A narrower route takes over the pending request before the broad one answers.
	const narrow = origin.requestsRoute(Path.from("live"));
	const { value: req } = await narrow.requested().next();
	stale?.reject(new Error("unserved"));
	await settle();
	expect(request.closed.peek()).toBeUndefined();

	const produced = new BroadcastProducer();
	hooks.acceptRequest(req, produced);
	await settle();
	expect(request.active.peek()).toBeDefined();

	request.close();
	void wideRequests.return?.();
	narrow.close();
	wide.close();
	produced.close();
	origin.close();
});

test("a refusal from a route a local broadcast superseded does not end the request", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const wide = origin.requestsRoute(Path.from("live"));
	const wideRequests = wide.requested();

	const request = consumer.request(Path.from("live/cam"));
	const { value: stale } = await wideRequests.next();

	// The exact local broadcast takes over before the broad route answers.
	const local = publish(origin, Path.from("live/cam"));
	await settle();
	const active = request.active.peek();
	expect(active).toBeDefined();

	stale?.reject(new Error("unserved"));
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	expect(request.active.peek()).toBe(active);

	request.close();
	void wideRequests.return?.();
	wide.close();
	local.close();
	origin.close();
});

test("close rejects queued requests as unroutable", async () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"));
	const waiting = handle.requested().next();
	const request = origin.request(Path.from("live/cam"));
	const { value: req } = await waiting;
	expect(req).toBeDefined();

	handle.close();
	await settle();

	expect((await handle.requested().next()).done).toBe(true);
	expect(request.active.peek()).toBeUndefined();
	hooks.acceptRequest(req, new BroadcastProducer());
	await settle();
	expect(request.active.peek()).toBeUndefined();

	request.close();
	origin.close();
});

test("announced filters by arbitrary patterns and reports captures", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const broadcast = publish(origin, Path.from("room/alice"));
	const announced = consumer.announced(Path.Pattern.parse("room/*"));

	const update = await announced.next();
	expect(update?.prefix).toBe(Path.from("room/alice"));
	expect(update?.captures?.map((capture) => capture.text)).toEqual(["alice"]);

	announced.close();
	broadcast.close();
	origin.close();
});

test("the spread hash matches rs/moq-net byte for byte", () => {
	expect(spreadHash("pool/job-0", [10n])).toBe(0xefb5e20a66101c32n);
	expect(spreadHash("pool/job-0", [11n])).toBe(0x0eb0a91370ff6653n);
});

test("an equal-cost pool spreads its paths the same way on every node", async () => {
	const workers = [10n, 11n, 12n, 13n].map((id) => HopSchema.parse(id));
	const paths = Array.from({ length: 64 }, (_, i) => Path.from(`pool/job-${i}`));

	// The worker each path resolves to on an origin whose pool arrived in `order`.
	async function winners(order: typeof workers): Promise<bigint[]> {
		const origin = new Producer();
		const served = new Map<Path.Valid, bigint>();
		const handles = order.map((hop) => {
			const handle = wireOf(origin).receive(Path.from("pool"), { hops: [hop], cost: 3n });
			void (async () => {
				for await (const request of handle.requested()) {
					served.set(request.path, hop);
					hooks.acceptRequest(request, new BroadcastProducer());
				}
			})();
			return handle;
		});
		const requests = paths.map((path) => origin.request(path));
		await settle();
		for (const request of requests) request.close();
		for (const handle of handles) handle.close();
		origin.close();
		return paths.map((path) => served.get(path) ?? -1n);
	}

	const forward = await winners(workers);
	const reverse = await winners([...workers].reverse());
	expect(reverse).toEqual(forward);

	// Not an assertion about any two paths, which a correct hash may put on one worker:
	// only that the set does not pile onto a few.
	for (const worker of workers) {
		expect(forward.filter((hop) => hop === worker).length).toBeGreaterThanOrEqual(paths.length / 16);
	}
});

// A publisher that reconnects under a new hop while its old session lingers takes over at once,
// whichever hop hashes lower: only a prefix's paths are spread by the hash.
test("an equal-cost replacement at its own path wins whatever its hop", async () => {
	const path = Path.from("pool/job-0");
	for (const order of [
		[11n, 10n],
		[10n, 11n],
	].map((hops) => hops.map((id) => HopSchema.parse(id)))) {
		const origin = new Producer();
		const served: bigint[] = [];
		const handles = order.map((hop) => {
			const handle = wireOf(origin).receive(path, { hops: [hop], cost: 3n });
			void (async () => {
				for await (const request of handle.requested()) {
					served.push(hop);
					hooks.acceptRequest(request, new BroadcastProducer());
				}
			})();
			return handle;
		});
		await settle();
		expect((await announcedView(origin.consume())).peek().get(path)?.hops).toEqual([order[1]]);
		const request = origin.request(path);
		await settle();
		expect(served).toEqual([order[1]]);
		request.close();
		for (const handle of handles) handle.close();
		origin.close();
	}
});

test("a serving session closing releases quiet origin change listeners", async () => {
	const origin = new Producer();
	const changed = Signal.prototype.changed;
	let listeners = 0;
	const spy = spyOn(Signal.prototype, "changed").mockImplementation(function (
		this: Signal<unknown>,
		fn?: (value: unknown) => void,
	) {
		if (!fn) return new Promise((resolve) => changed.call(this, resolve));
		listeners++;
		let live = true;
		const dispose = changed.call(this, fn);
		return () => {
			if (!live) return;
			live = false;
			listeners--;
			dispose();
		};
	} as typeof changed);
	try {
		for (let i = 0; i < 10; i++) {
			const closed = new Once<null>();
			const pending = race([wireOf(origin).changed(), closed]);
			expect(listeners).toBe(5);
			closed.set(null);
			expect(await pending).toBeNull();
			expect(listeners).toBe(0);
		}
	} finally {
		spy.mockRestore();
		origin.close();
	}
});

test("a rooted producer shares the table and enforces its pattern union", async () => {
	const origin = new Producer();
	const scoped = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.parse("room/*")]));
	const broadcast = publish(scoped, Path.from("room/alice"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announcedView(origin)).peek().has(Path.from("tenant/room/alice"))).toBe(true);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(await announcedView(scoped)).peek().keys()]).toEqual([Path.from("room/alice")]);
	const request = scoped.consume().request(Path.from("room/alice"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(request.path).toBe(Path.from("room/alice"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(request.active.peek()).toBeDefined();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => scoped.createBroadcast(Path.from("other")).broadcast).toThrow("outside the origin scope");
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => scoped.request(Path.from("room/alice/deep"))).toThrow("outside the origin scope");
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => scoped.requestsRoute(Path.from("other"))).toThrow("outside the origin scope");
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => scoped.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("other/**")]))).toThrow(
		"do not overlap",
	);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => origin.scope(Path.empty(), new Path.Patterns())).toThrow("do not overlap");

	const nested = scoped.scope(Path.from("room"), new Path.Patterns([Path.Pattern.all()]));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(await announcedView(nested)).peek().keys()]).toEqual([Path.from("alice")]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => nested.createBroadcast(Path.from("alice/deep")).broadcast).toThrow("outside the origin scope");
	const announced = nested.announced();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(await announced.next()).toMatchObject({ kind: "start", prefix: Path.from("alice") });
	announced.close();
	request.close();
	broadcast.close();
	origin.close();
});

test("closing a derived handle releases only what it registered", async () => {
	const origin = new Producer();
	const owned = publish(origin, Path.from("lobby"));
	const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.subtree(Path.from("room"))]));
	const wide = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.all()]));
	const inside = publish(scoped, Path.from("room/alice"));
	const claim = scoped.requests(Path.from("room/claim"));
	const ranked = wide.requests(Path.from("ranked"));
	const peer = serve(scoped, Path.from("room/peer"), provider(new BroadcastProducer()));

	scoped.close();
	wide.close();

	expect(origin.closed.peek()).toBeUndefined();
	expect(owned.closed.peek()).toBeUndefined();
	expect(inside.closed.peek()).toBeNull();
	const routes = (await announcedView(origin)).peek();
	expect([...routes.keys()]).toEqual([Path.from("lobby")]);

	// A released handle refuses new work, so a borrower notices instead of publishing into nothing.
	expect(() => scoped.createBroadcast(Path.from("room/late")).broadcast).toThrow("origin handle is closed");
	scoped.requests(Path.from("room/late")).close();
	expect((await announcedView(origin)).peek().has(Path.from("room/late"))).toBe(false);

	// The owner and fresh handles keep working.
	const again = publish(origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.all()])), Path.from("room/bob"));
	expect((await announcedView(origin)).peek().has(Path.from("room/bob"))).toBe(true);

	// Closing what was already released is a no-op.
	claim.close();
	ranked.close();
	peer();
	scoped.close();
	again.close();
	origin.close();
	expect(owned.closed.peek()).toBeNull();
});

test("a broadcast a derived handle released cannot announce again", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.all()]));
	const path = Path.from("room");
	const publication = scoped.createBroadcast(path);
	const broadcast = publication.broadcast;
	publication.announce();
	const held = await routed(consumer, path);

	scoped.close();

	// The released handle closes what it created, whoever still holds a handle on it.
	expect(broadcast.closed.peek()).toBeNull();
	expect(() => publication.announce()).toThrow("broadcast is closed");
	expect((await announcedView(origin)).peek().has(path)).toBe(false);

	held?.close();
	origin.close();
});

test("closing a derived handle withdraws the sessions and answers attached through it", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.all()]));
	const path = Path.from("blind");

	const request = consumer.request(path);
	const detach = wireOf(scoped).attach(false);
	const snapshot = wireOf(scoped).snapshot(Path.empty());
	const interests: string[] = [];
	const unlisten = wireOf(scoped).interests((prefix, wanted) => interests.push(`${wanted ? "+" : "-"}${prefix}`));
	const announced = consumer.announced();
	const upstream = new BroadcastProducer();
	const withdraw = wireOf(scoped).answer(path, upstream.consume());
	expect(withdraw).toBeDefined();
	await settle();
	expect(request.active.peek()).toBeDefined();
	expect(consumer.discovery.peek()).toBe(false);
	expect(announced.synced.peek()).toBe(false);
	// The request owns its path's discovery until the root watch covers it.
	expect(interests.splice(0)).toEqual(["+blind", "+", "-blind"]);

	scoped.close();
	await settle();

	// The session stops asking its peer for announcements, now and for later watches.
	expect(interests.splice(0)).toEqual(["-"]);
	consumer.request(Path.from("later"), { announced: true }).close();
	expect(interests).toEqual([]);

	// The answer, the attached session, and the initial set in flight went with the handle.
	expect(request.active.peek()).toBeUndefined();
	expect(consumer.discovery.peek()).toBeUndefined();
	expect(announced.synced.peek()).toBe(true);
	const retry = consumer.request(path);

	// A session still using the released handle cannot answer, count as attached, or
	// hold readers unsynced.
	expect(wireOf(scoped).answer(path, new BroadcastProducer().consume())).toBeUndefined();
	wireOf(scoped).attach(false);
	wireOf(scoped).snapshot(Path.empty());
	await settle();
	expect(retry.active.peek()).toBeUndefined();
	expect(consumer.discovery.peek()).toBeUndefined();
	expect(announced.synced.peek()).toBe(true);

	// The session's own teardown afterwards is a no-op.
	withdraw?.();
	snapshot();
	unlisten();
	expect(interests).toEqual([]);
	detach();
	announced.close();
	retry.close();
	request.close();
	origin.close();
});

test("a scoped dynamic only serves allowed paths and presents relative requests", async () => {
	const origin = new Producer();
	const scoped = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.parse("room/*/chat")]));
	const dynamic = scoped.requestsRoute(Path.from("room"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(dynamic.prefix).toBe(Path.from("room"));
	const denied = origin.request(Path.from("tenant/room/alice/video"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(denied.active.peek()).toBeUndefined();
	const request = origin.request(Path.from("tenant/room/alice/chat"));
	const requests = dynamic.requested();
	const pending = await requests.next();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(pending.value?.path).toBe(Path.from("room/alice/chat"));
	const broadcast = new BroadcastProducer();
	hooks.acceptRequest(pending.value, broadcast);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(request.active.peek()).toBeDefined();
	dynamic.updateRoute({ cost: 2n });
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announcedView(origin)).peek().get(Path.from("tenant/room"))?.cost).toBe(2n);
	await requests.return?.();
	dynamic.close();
	request.close();
	denied.close();
	broadcast.close();
	origin.close();
});

test("scoped wire views filter and rebase advertisements and blind requests", () => {
	const origin = new Producer();
	const scoped = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.parse("room/**")]));
	const local = publish(origin, Path.from("tenant/room/live"));
	const hidden = publish(origin, Path.from("tenant/other/live"));
	const allowed = origin.request(Path.from("tenant/room/missing"));
	const denied = origin.request(Path.from("tenant/other/missing"));
	expect([...(wireOf(scoped).requests.peek()?.keys() ?? [])]).toEqual([Path.from("room/missing")]);
	expect([...(wireOf(scoped.consume()).advertised.peek()?.keys() ?? [])]).toEqual([Path.from("room/live")]);
	expect(wireOf(scoped.consume()).local(Path.from("room/live"))).toBeDefined();
	expect(() => wireOf(scoped.consume()).local(Path.from("other/live"))).toThrow("outside the origin scope");
	allowed.close();
	denied.close();
	local.close();
	hidden.close();
	origin.close();
});

test("a scoped dynamic is announced only to readers its scope can serve", async () => {
	const origin = new Producer();
	const dynamic = origin
		.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("*/chat")]))
		.requestsRoute(Path.empty());
	const chat = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/chat")]));
	const video = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/video")]));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(await announcedView(chat)).peek().keys()]).toEqual([Path.empty()]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(await announcedView(video)).peek().keys()]).toEqual([]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(wireOf(chat.consume()).advertised.peek()?.keys() ?? [])]).toEqual([Path.empty()]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(wireOf(video.consume()).advertised.peek()?.keys() ?? [])]).toEqual([]);
	dynamic.close();
	origin.close();
});

test("a rooted reader presents the most specific covering route", async () => {
	const origin = new Producer();
	const narrow = origin.requestsRoute(Path.from("room/alice"), { cost: 9n });
	const broad = origin.requestsRoute(Path.from("room"), { cost: 1n });
	const rooted = origin.scope(Path.from("room/alice"), new Path.Patterns([Path.Pattern.all()]));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announcedView(rooted)).peek().get(Path.empty())?.cost).toBe(9n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(wireOf(rooted.consume()).advertised.peek()?.get(Path.empty())?.[0]?.route.cost).toBe(9n);
	broad.close();
	narrow.close();
	origin.close();
});

test("a scoped reader picks the best route its scope can see at a prefix", async () => {
	const origin = new Producer();
	const scoped = (pattern: string) => origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse(pattern)]));
	const chat = scoped("*/chat").requestsRoute(Path.empty(), { cost: 1n });
	const video = scoped("*/video").requestsRoute(Path.empty(), { cost: 5n });
	const reader = scoped("room/video");

	await new Promise<void>((resolve) => setImmediate(resolve));

	expect((await announcedView(reader)).peek().get(Path.empty())?.cost).toBe(5n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announcedView(origin, Path.Pattern.parse("room/video"))).peek().get(Path.empty())?.cost).toBe(5n);
	const announced = reader.announced();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announced.next())?.route.cost).toBe(5n);
	announced.close();

	// A session publishing the scoped view offers the video route too.
	const offered = [...(wireOf(reader.consume()).advertised.peek()?.get(Path.empty()) ?? [])];
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(offered.map((advert) => advert.route.cost)).toEqual([5n]);

	video.close();
	chat.close();
	origin.close();
});

test("a scoped reader sees a local broadcast that only loses to a route outside its scope", async () => {
	const origin = new Producer();
	const chat = origin
		.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/*/chat")]))
		.requestsRoute(Path.from("room/alice"), { epoch: EPOCH, cost: 0n });
	const localPublication = origin.createBroadcast(Path.from("room/alice"));
	const local = localPublication.broadcast;
	localPublication.announce({ epoch: EPOCH, cost: 5n });
	const reader = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/*")]));

	await new Promise<void>((resolve) => setImmediate(resolve));

	expect((await announcedView(reader)).peek().get(Path.from("room/alice"))?.cost).toBe(5n);
	// Unscoped, the cheaper route still wins the prefix.
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announcedView(origin)).peek().get(Path.from("room/alice"))?.cost).toBe(0n);

	local.close();
	chat.close();
	origin.close();
});

test("broadcast handles carry the path they were created or requested at", async () => {
	expect(new BroadcastProducer().consume().path).toBe(Path.empty());

	const origin = new Producer();
	const scoped = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.parse("room/*")]));
	const broadcast = publish(scoped, Path.from("room/alice"));
	expect(broadcast.consume().path).toBe(Path.from("tenant/room/alice"));

	// Relative to each cursor's root, and kept by a clone.
	const whole = origin.request(Path.from("tenant/room/alice"));
	const rooted = scoped.consume().request(Path.from("room/alice"));
	expect(whole.active.peek()?.path).toBe(Path.from("tenant/room/alice"));
	expect(rooted.active.peek()?.path).toBe(Path.from("room/alice"));
	const clone = rooted.active.peek()?.clone();
	expect(clone?.path).toBe(Path.from("room/alice"));

	// A dynamic handler's standalone broadcast is named by the request, too.
	const dynamic = origin.requestsRoute(Path.from("live"));
	const request = origin.request(Path.from("live/bob"));
	const pending = await dynamic.requested().next();
	const served = new BroadcastProducer();
	hooks.acceptRequest(pending.value, served);
	expect(request.active.peek()?.path).toBe(Path.from("live/bob"));

	clone?.close();
	whole.close();
	rooted.close();
	request.close();
	served.close();
	dynamic.close();
	broadcast.close();
	origin.close();
});

test("the newest epoch wins the path over a cheaper one and arrives as a new broadcast", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room/alice");
	const announced = consumer.announced();

	const oldPublication = origin.createBroadcast(path);
	const old = oldPublication.broadcast;
	oldPublication.announce({ epoch: Epoch.mint(), cost: 1n });
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });

	// Minted after the local broadcast's, so it is the newer publisher despite the cost.
	const newer = wireOf(origin).receive(path, { epoch: Epoch.mint(), hops: [PEER], cost: 9n });
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart", route: { hops: [PEER] } });

	newer.close();
	old.close();
	announced.close();
	origin.close();
});

const NEWER = Epoch.parse("01900000-0000-7000-8000-000000000002");

test("demand held across an epoch change asks again, unless it named the old epoch", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("live/cam");
	const handle = origin.requestsRoute(Path.from("live"), { epoch: EPOCH });
	const it = handle.requested();
	const pinned = wireOf(consumer).demand(path, EPOCH);
	const unpinned = wireOf(consumer).demand(path);
	const { value: req } = await it.next();

	// Another instance now serves the route: the old handler's answer is for the old one.
	handle.updateRoute({ ...handle.route, epoch: NEWER });
	hooks.acceptRequest(req, new BroadcastProducer().consume());
	await expect(pinned).rejects.toThrow("unroutable");

	const next = new BroadcastProducer();
	const track = next.createTrack("video");
	hooks.acceptRequest((await it.next()).value, next);
	const subscription = (await unpinned)?.track("video").subscribe();
	track.createGroup(7);
	expect((await subscription?.recvGroup())?.sequence).toBe(7);

	subscription?.close();
	await it.return?.();
	next.close();
	handle.close();
	origin.close();
});

test("a demand refused across an epoch change does not lose the carried demand's answer", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("live/cam");
	const handle = origin.requestsRoute(Path.from("live"), { epoch: EPOCH });
	const it = handle.requested();
	const unpinned = wireOf(consumer).demand(path);
	const pinned = wireOf(consumer).demand(path, EPOCH);
	await it.next();

	handle.updateRoute({ ...handle.route, epoch: NEWER });
	await expect(pinned).rejects.toThrow("unroutable");
	// The replacement's refusal reaches the demand that carried over.
	(await it.next()).value?.reject(new Error("gone"));
	await expect(unpinned).rejects.toThrow("gone");

	await it.return?.();
	handle.close();
	origin.close();
});

test("demand held across an epoch round trip still asks again", async () => {
	const origin = new Producer();
	const path = Path.from("live/cam");
	const handle = origin.requestsRoute(Path.from("live"), { epoch: EPOCH });
	const it = handle.requested();
	const demanded = wireOf(origin.consume()).demand(path);
	const { value: req } = await it.next();

	// Released by the first change; the epoch coming back does not undo that.
	handle.updateRoute({ ...handle.route, epoch: NEWER });
	handle.updateRoute({ ...handle.route, epoch: EPOCH });
	hooks.acceptRequest(req, new BroadcastProducer().consume());

	const next = new BroadcastProducer();
	const track = next.createTrack("video");
	hooks.acceptRequest((await it.next()).value, next);
	const subscription = (await demanded)?.track("video").subscribe();
	track.createGroup(7);
	expect((await subscription?.recvGroup())?.sequence).toBe(7);

	subscription?.close();
	await it.return?.();
	next.close();
	handle.close();
	origin.close();
});

test("re-pricing from the current route keeps the epoch", () => {
	const origin = new Producer();
	const handle = origin.requestsRoute(Path.from("live"), { epoch: EPOCH, cost: 5n });
	handle.updateRoute({ ...handle.route, cost: 1n });
	expect(handle.route).toEqual(Route.normalize({ epoch: EPOCH, cost: 1n }));

	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	expect(broadcastPublication.route).toBeUndefined();
	broadcastPublication.announce({ epoch: EPOCH, cost: 5n });
	broadcastPublication.announce({ ...broadcastPublication.route, cost: 1n });
	expect(broadcastPublication.route).toEqual(Route.normalize({ epoch: EPOCH, cost: 1n }));
	broadcastPublication.unannounce();
	expect(broadcastPublication.route).toBeUndefined();

	broadcast.close();
	handle.close();
	origin.close();
});

test("a peer naming another epoch is refused rather than served a different instance", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const localPublication = origin.createBroadcast(path);
	const local = localPublication.broadcast;
	localPublication.announce({ epoch: EPOCH });

	expect(wireOf(consumer).local(path, EPOCH)).toBeDefined();
	const other = Epoch.parse("01900000-0000-7000-8000-000000000002");
	expect(wireOf(consumer).local(path, other)).toBeUndefined();
	await expect(wireOf(consumer).demand(path, other)).rejects.toThrow("unroutable");

	local.close();
	origin.close();
});

test("a newer epoch ends pinned and unpinned requests on the replaced publisher alike", () => {
	const origin = new Producer();
	const path = Path.from("room");
	const first = origin.createBroadcast(path);
	first.announce({ epoch: EPOCH });
	const reader = origin.consume();
	const pinned = reader.request(path, { epoch: EPOCH });
	const unpinned = reader.request(path);
	expect(pinned.active.peek()?.epoch).toBe(EPOCH);
	expect(unpinned.active.peek()?.epoch).toBe(EPOCH);
	const newerEpoch = Epoch.parse("01900000-0000-7000-8000-000000000002");
	const second = origin.createBroadcast(path);
	second.announce({ epoch: newerEpoch });
	expect(() => first.announce({ epoch: EPOCH })).toThrow("broadcast is closed");
	first.unannounce();
	expect(pinned.active.peek()).toBeUndefined();
	// The replaced publisher closed: the unpinned request ends rather than move to the newer one.
	expect(unpinned.active.peek()).toBeUndefined();
	// A fresh unpinned request names the publisher it actually resolved.
	const fresh = reader.request(path);
	expect(fresh.active.peek()?.epoch).toBe(newerEpoch);
	fresh.close();
	const stale = reader.request(path, { epoch: EPOCH });
	expect(stale.active.peek()).toBeUndefined();
	stale.close();
	pinned.close();
	unpinned.close();
	origin.close();
});

test("a request held across an epoch change asks again and never takes the old handler's answer", async () => {
	const origin = new Producer();
	const prefix = Path.from("room");
	const path = Path.from("room/cam");
	const dynamic = origin.requestsRoute(prefix, { epoch: EPOCH });
	const unpinned = origin.consume().request(path);
	const pinned = origin.consume().request(path, { epoch: EPOCH });
	const requests = dynamic.requested();
	const old = (await requests.next()).value;
	dynamic.updateRoute({ ...dynamic.route, epoch: NEWER });
	const current = origin.consume().request(path, { epoch: NEWER });
	hooks.acceptRequest(old, new BroadcastProducer().consume());
	await settle();
	expect(pinned.active.peek()).toBeUndefined();
	expect(unpinned.active.peek()).toBeUndefined();
	expect(current.active.peek()).toBeUndefined();
	const next = (await requests.next()).value;
	hooks.acceptRequest(next, new BroadcastProducer().consume());
	await settle();
	expect(unpinned.active.peek()?.epoch).toBe(NEWER);
	expect(current.active.peek()?.epoch).toBe(NEWER);
	expect(pinned.active.peek()).toBeUndefined();
	await requests.return?.();
	unpinned.close();
	pinned.close();
	current.close();
	dynamic.close();
	origin.close();
});

test.each([
	["A to B", EPOCH, NEWER],
	["A to none", EPOCH, undefined],
	["none to B", undefined, NEWER],
])("an epoch update %s restarts the claim, while a re-price keeps every handle", async (_, from, to) => {
	const origin = new Producer();
	const prefix = Path.from("live");
	const path = Path.from("live/cam");
	const dynamic = origin.requestsRoute(prefix, { epoch: from });
	const requests = dynamic.requested();
	const announced = origin.consume().announced();
	expect(await announced.next()).toMatchObject({ prefix, kind: "start" });

	const held = origin.consume().request(path);
	const old = new BroadcastProducer();
	const oldTrack = old.createTrack("video");
	hooks.acceptRequest((await requests.next()).value, old);
	await settle();
	const resolved = held.active.peek();
	expect(resolved?.epoch).toBe(from);

	const sticky = resolved?.track("video").subscribe();
	oldTrack.appendGroup().finish();
	expect((await sticky?.recvGroup())?.sequence).toBe(0);

	// A re-price at the same epoch keeps every handle, and the handler is not asked again.
	dynamic.updateRoute({ ...dynamic.route, cost: 5n });
	expect(await announced.next()).toMatchObject({ prefix, kind: "update" });
	expect(held.active.peek()).toBe(resolved);
	const same = origin.consume().request(path);
	await settle();
	expect(same.active.peek()?.epoch).toBe(from);
	const pinned = origin.consume().request(path, { epoch: from });

	// Another epoch, or none, is another instance: resolved requests keep the old one.
	dynamic.updateRoute({ ...dynamic.route, epoch: to });
	expect(await announced.next()).toMatchObject({ prefix, kind: "restart" });
	await settle();
	expect(held.active.peek()).toBe(resolved);
	expect(held.closed.peek()).toBeUndefined();
	expect(same.active.peek()?.epoch).toBe(from);
	expect(pinned.active.peek()?.epoch).toBe(from);

	// The subscription already open stays on the old answer.
	oldTrack.appendGroup().finish();
	expect((await sticky?.recvGroup())?.sequence).toBe(1);

	// A re-request, local or from a session, asks the handler again and never reads the
	// old answer.
	const fresh = origin.consume().request(path);
	const demanded = wireOf(origin.consume()).demand(path);
	const next = new BroadcastProducer();
	const nextTrack = next.createTrack("video");
	hooks.acceptRequest((await requests.next()).value, next);
	await settle();
	expect(fresh.active.peek()?.epoch).toBe(to);
	const subscription = fresh.active.peek()?.track("video").subscribe();
	const forwarded = (await demanded)?.track("video").subscribe();
	nextTrack.createGroup(7);
	expect((await subscription?.recvGroup())?.sequence).toBe(7);
	expect((await forwarded?.recvGroup())?.sequence).toBe(7);

	// Existing request handles can still subscribe, but never join the new answer.
	const retained = held.active.peek()?.track("video").subscribe();
	expect((await retained?.recvGroup())?.sequence).toBe(1);
	oldTrack.appendGroup().finish();
	expect((await retained?.recvGroup())?.sequence).toBe(2);
	expect(held.active.peek()).toBe(resolved);

	// Retracting the new claim does not retract the resolved old request.
	dynamic.close();
	await settle();
	expect(held.active.peek()).toBe(resolved);
	expect(held.closed.peek()).toBeUndefined();
	retained?.close();
	await requests.return?.();
	sticky?.close();
	subscription?.close();
	forwarded?.close();
	for (const handle of [held, same, pinned, fresh]) handle.close();
	old.close();
	next.close();
	dynamic.close();
	announced.close();
	origin.close();
});

test("an epoch update does not retain an answer that already closed", async () => {
	const origin = new Producer();
	const path = Path.from("live/cam");
	const dynamic = origin.requestsRoute(Path.from("live"), { epoch: EPOCH });
	const requests = dynamic.requested();
	const held = origin.consume().request(path);
	const old = new BroadcastProducer();
	hooks.acceptRequest((await requests.next()).value, old);
	old.close();
	// The server's closure callback has not removed the answer yet.
	dynamic.updateRoute({ epoch: NEWER });
	try {
		expect(held.closed.peek()).toBeInstanceOf(Error);
		expect(held.active.peek()).toBeUndefined();
		const fresh = origin.consume().request(path);
		const next = new BroadcastProducer();
		hooks.acceptRequest((await requests.next()).value, next);
		expect(fresh.active.peek()?.epoch).toBe(NEWER);
		fresh.close();
		next.close();
	} finally {
		held.close();
		await requests.return?.();
		dynamic.close();
		origin.close();
	}
});

test("an unread resolved request retains its broadcast until its last handle closes", async () => {
	const origin = new Producer();
	const path = Path.from("live/cam");
	const dynamic = origin.requestsRoute(Path.from("live"), { epoch: EPOCH });
	const requests = dynamic.requested();
	const held = origin.consume().request(path);
	const old = new SessionBroadcast();
	hooks.acceptRequest((await requests.next()).value, old);
	// Change epochs before the active getter or its subscription can clone the answer.
	dynamic.updateRoute({ epoch: NEWER });
	expect(old.closed.peek()).toBeUndefined();
	expect(held.active.peek()?.epoch).toBe(EPOCH);
	held.close();
	await old.closed;
	expect(held.active.peek()).toBeUndefined();
	await requests.return?.();
	dynamic.close();
	origin.close();
});

test("a session's re-request after an epoch update asks the handler again", async () => {
	const origin = new Producer();
	const path = Path.from("live/cam");
	const dynamic = origin.requestsRoute(Path.from("live"), { epoch: EPOCH });
	const requests = dynamic.requested();
	const wire = wireOf(origin.consume());

	const before = wire.demand(path);
	const old = new BroadcastProducer();
	hooks.acceptRequest((await requests.next()).value, old);
	expect(await before).toBeDefined();

	dynamic.updateRoute({ ...dynamic.route, epoch: NEWER });
	const after = wire.demand(path);
	const next = new BroadcastProducer();
	const track = next.createTrack("video");
	hooks.acceptRequest((await requests.next()).value, next);
	const subscription = (await after)?.track("video").subscribe();
	track.createGroup(7);
	expect((await subscription?.recvGroup())?.sequence).toBe(7);

	subscription?.close();
	await requests.return?.();
	old.close();
	next.close();
	dynamic.close();
	origin.close();
});

test("an update does not override route selection: another claim at the epoch keeps it", async () => {
	const origin = new Producer();
	const prefix = Path.from("live");
	const path = Path.from("live/cam");
	const first = origin.requestsRoute(prefix, { epoch: EPOCH, cost: 1n });
	const second = origin.requestsRoute(prefix, { epoch: EPOCH, cost: 5n });
	const firstRequests = first.requested();
	const secondRequests = second.requested();

	const held = origin.consume().request(path);
	const answer = new BroadcastProducer();
	hooks.acceptRequest((await firstRequests.next()).value, answer);
	await settle();
	expect(held.active.peek()?.epoch).toBe(EPOCH);

	// The first claim drops the epoch, so the second, still at it, outranks it: the
	// resolved request keeps its answer, and new requests go through the second claim.
	first.updateRoute({ ...first.route, epoch: undefined });
	const fresh = origin.consume().request(path);
	const replacement = new BroadcastProducer();
	const track = replacement.createTrack("video");
	hooks.acceptRequest((await secondRequests.next()).value, replacement);
	await settle();
	expect(held.active.peek()?.epoch).toBe(EPOCH);
	expect(fresh.active.peek()?.epoch).toBe(EPOCH);
	const subscription = fresh.active.peek()?.track("video").subscribe();
	track.createGroup(7);
	expect((await subscription?.recvGroup())?.sequence).toBe(7);

	subscription?.close();
	await firstRequests.return?.();
	await secondRequests.return?.();
	held.close();
	fresh.close();
	answer.close();
	replacement.close();
	first.close();
	second.close();
	origin.close();
});

test("a route without an epoch resolves a handle without one", () => {
	const origin = new Producer();
	const path = Path.from("room");
	const broadcast = origin.createBroadcast(path);
	broadcast.announce();
	const request = origin.consume().request(path);
	expect(request.active.peek()).toBeDefined();
	expect(request.active.peek()?.epoch).toBeUndefined();
	request.close();
	origin.close();
});

test("public announcements saturate static costs and refuse invalid cost shapes", async () => {
	const origin = new Producer();
	const path = Path.from("priced");
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	const view = await announcedView(origin);
	broadcastPublication.announce({ cost: 2n ** 64n - 1n });
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(view.peek().get(path)?.cost).toBe(2n ** 62n - 1n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => broadcastPublication.announce({ cost: -1n })).toThrow(RangeError);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => broadcastPublication.announce({ cost: { warm: 1n, cold: 2n } } as unknown as Route)).toThrow(
		RangeError,
	);
	broadcast.close();
	origin.close();
});

test("published names refuse navigation segments", () => {
	const origin = new Producer();
	try {
		for (const name of ["room/./cam", "room/../cam", ".", ".."]) {
			const path = Path.from(name);
			expect(() => origin.createBroadcast(path).broadcast).toThrow();
			expect(() => origin.requests(path)).toThrow();
			expect(() => wireOf(origin).receive(path, Route.default)).toThrow();
		}
		const scoped = origin.scope(Path.from("room/.."), new Path.Patterns([Path.Pattern.all()]));
		expect(() => scoped.createBroadcast(Path.from("cam")).broadcast).toThrow();
		for (const path of ["room/.pro/cam", "room/a..b", "room/cam"]) {
			origin.createBroadcast(Path.from(path)).broadcast.close();
		}
	} finally {
		origin.close();
	}
});

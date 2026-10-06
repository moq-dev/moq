import { expect, spyOn, test } from "bun:test";
import { getter, Once, race, Signal } from "@moq/signals";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { StreamCode, StreamError } from "./error.ts";
import { HopSchema, Route, stampHops } from "./hop.ts";
import type { Consumer, Table } from "./origin.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";
import { wireOf } from "./wire.ts";

/** The next route event, skipping the live marker: these tests pin routes, and the marker has its own. */
async function nextRoute<E extends { kind: string }>(announced: {
	next(): Promise<E | undefined>;
}): Promise<Exclude<E, { kind: "live" }> | undefined> {
	for (;;) {
		const event = await announced.next();
		if (event?.kind !== "live") return event as Exclude<E, { kind: "live" }> | undefined;
	}
}

function publish(origin: Producer, path: Path.Valid) {
	const broadcast = origin.createBroadcast(path);
	broadcast.announce();
	return broadcast;
}

/** A peer's hop, for a route that is not local. */
const PEER = HopSchema.parse(10n);

/** Land a received prefix, served from `consume`, the way a session does. */
function serve(origin: Producer, prefix: Path.Valid, consume: () => BroadcastConsumer, route: Route = Route.default) {
	const handle = wireOf(origin).receive(prefix, route);
	void (async () => {
		try {
			for await (const request of handle.requested()) {
				request.accept(consume());
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
	const live = table.broadcasts(scope);
	const changes: ReadonlyMap<Path.Valid, Route>[] = [];
	const stop = live.subscribe((value) => changes.push(value));
	expect(live.peek().size).toBe(0);

	const other = publish(origin, Path.from("other"));
	const localPath = Path.from("room/alice");
	const local = table.createBroadcast(localPath);
	expect(live.peek().has(localPath)).toBe(false);
	expect(live.peek().has(Path.from("other"))).toBe(false);

	local.announce({ cost: 4n });
	await settle();
	expect(live.peek().get(localPath)).toEqual(Route.normalize({ cost: 4n }));
	expect(changes.some((value) => value.get(localPath)?.cost.warm === 4n)).toBe(true);

	const prefix = Path.from("room");
	const dynamic = table.dynamic(prefix, { cost: 2n });
	expect(live.peek().get(prefix)).toEqual(Route.normalize({ cost: 2n }));
	dynamic.update({ cost: 3n });
	await settle();
	expect(live.peek().get(prefix)).toEqual(Route.normalize({ cost: 3n }));

	dynamic.close();
	local.close();
	other.close();
	await settle();
	expect(live.peek().size).toBe(0);
	stop();
	origin.close();
});

test("unscoped broadcast getters share one fresh snapshot per mutation", () => {
	const origin = new Producer();
	const first = origin.broadcasts();
	const second = origin.consume().broadcasts();
	const empty = first.peek();
	expect(second.peek()).toBe(empty);

	const path = Path.from("room/alice");
	const handle = origin.dynamic(path, { cost: 1n });
	const added = second.peek();
	expect(added).not.toBe(empty);
	expect(first.peek()).toBe(added);
	expect(added.get(path)).toEqual(Route.normalize({ cost: 1n }));

	handle.update({ cost: 2n });
	const repriced = first.peek();
	expect(repriced).not.toBe(added);
	expect(second.peek()).toBe(repriced);
	expect(repriced.get(path)).toEqual(Route.normalize({ cost: 2n }));

	handle.close();
	origin.close();
});

test("a stamped route ranks below an identified one of the same length", () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const stamped = stampHops([], HopSchema.parse(5n));
	if (!stamped) throw new Error("an empty chain always has room");
	const identified = [HopSchema.parse(11n), PEER];
	expect(stamped).toHaveLength(identified.length);

	const legacy = wireOf(origin).receive(path, { hops: stamped, cost: 0n });
	const named = wireOf(origin).receive(path, { hops: identified, cost: 9n });
	expect(origin.consume().broadcasts().peek().get(path)).toEqual(Route.normalize({ hops: identified, cost: 9n }));

	legacy.close();
	named.close();
	origin.close();
});

test("an announced local path competes with a received route on cost", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room/alice");
	const remote = wireOf(origin).receive(path, { hops: [PEER], cost: 5n });
	const live = consumer.broadcasts();
	expect(live.peek().get(path)).toEqual(Route.normalize({ hops: [PEER], cost: 5n }));

	// Unannounced, it competes for nothing.
	const local = origin.createBroadcast(path);
	expect(live.peek().get(path)).toEqual(Route.normalize({ hops: [PEER], cost: 5n }));

	// Cheaper wins; a tie falls to the local broadcast.
	local.announce({ cost: 9n });
	expect(live.peek().get(path)).toEqual(Route.normalize({ hops: [PEER], cost: 5n }));
	local.announce({ cost: 5n });
	expect(live.peek().get(path)).toEqual(Route.normalize({ cost: 5n }));

	local.close();
	await settle();
	expect(live.peek().get(path)).toEqual(Route.normalize({ hops: [PEER], cost: 5n }));

	remote.close();
	await settle();
	expect(live.peek().size).toBe(0);
	origin.close();
	expect(live.peek().size).toBe(0);
});

test("a scoped route remains visible when an exact local path is outside the scope", () => {
	const origin = new Producer();
	const path = Path.from("room");
	const remote = wireOf(origin).receive(path, { cost: 5n });
	const local = publish(origin, path);
	const scope = Path.Pattern.parse("room/*");
	const live = origin.broadcasts(scope);

	expect(origin.broadcasts().peek().get(path)).toEqual(Route.default);
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
	broadcast.createTrack("video");

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

test("a consumer clone keeps a superseded broadcast alive", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const first = publish(origin, path);
	const mine = await routed(consumer, path);
	expect(mine).toBeDefined();

	publish(origin, path);
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

test("the table is reactive", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const changed = wireOf(consumer).broadcasts.changed();
	const broadcast = publish(origin, path);

	const table = await changed;
	expect(table?.has(path)).toBe(true);

	broadcast.close();
	origin.close();
});

/** Whether `next` is still waiting once everything queued has run. */
async function pending(next: Promise<unknown>): Promise<boolean> {
	const waiting = Symbol("waiting");
	const result = await Promise.race([next, settle().then(() => waiting)]);
	return result === waiting;
}

test("an empty origin is live at once, and the marker never repeats", async () => {
	const origin = new Producer();
	const announced = origin.announced();
	expect(await announced.next()).toEqual({ kind: "live" });

	// Later routes are live changes, even past a session that replays and lands.
	const landed = wireOf(origin).replaying(Path.empty());
	landed();
	const alice = publish(origin, Path.from("alice"));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("alice"), kind: "start" });
	expect(await pending(announced.next())).toBe(true);

	alice.close();
	origin.close();
});

test("live follows the replayed routes", async () => {
	const origin = new Producer();
	const b = publish(origin, Path.from("b"));
	const c = publish(origin, Path.from("c"));

	const announced = origin.announced();
	const seen: string[] = [];
	for (;;) {
		const event = await announced.next();
		if (event?.kind === "live") break;
		expect(event?.kind).toBe("start");
		if (event?.kind === "start") seen.push(event.prefix);
	}
	expect(seen.sort()).toEqual(["b", "c"]);

	b.close();
	c.close();
	origin.close();
});

test("a replaying session withholds live until it lands", async () => {
	const origin = new Producer();
	const first = wireOf(origin).replaying(Path.empty());
	const second = wireOf(origin).replaying(Path.empty());
	const announced = origin.announced();

	// Routes a session lands arrive before the marker.
	serve(origin, Path.from("room/alice"), () => new BroadcastProducer().consume());
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room/alice"), kind: "start" });

	// Every session replaying when the stream opened has to land, and a release is idempotent.
	first();
	first();
	const next = announced.next();
	expect(await pending(next)).toBe(true);

	// A session that starts replaying after the stream opened is not waited on.
	const late = wireOf(origin).replaying(Path.empty());
	second();
	expect(await next).toEqual({ kind: "live" });

	late();
	announced.close();
	origin.close();
});

test("a stream waits only on sessions replaying into its scope", async () => {
	const origin = new Producer();
	const room = wireOf(origin).replaying(Path.from("room"));
	const other = wireOf(origin).replaying(Path.from("other"));

	// Nothing replays under `lobby`, and `other` cannot announce into `room/**`.
	const lobby = origin.announced(Path.Pattern.parse("lobby/**"));
	expect(await lobby.next()).toEqual({ kind: "live" });
	const announced = origin.announced(Path.Pattern.parse("room/**"));
	const next = announced.next();
	expect(await pending(next)).toBe(true);
	room();
	expect(await next).toEqual({ kind: "live" });

	// A scoped handle rebases the prefix under its root before comparing.
	const scoped = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.all()]));
	const inner = wireOf(scoped).replaying(Path.from("room"));
	const outer = origin.announced(Path.Pattern.parse("room/**"));
	expect(await outer.next()).toEqual({ kind: "live" });
	const tenant = origin.announced(Path.Pattern.parse("tenant/room/**"));
	expect(await pending(tenant.next())).toBe(true);

	inner();
	other();
	for (const stream of [lobby, announced, outer, tenant]) stream.close();
	origin.close();
});

test("announced streams the table under a scope with origin-relative paths", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const a = publish(origin, Path.from("room/a"));

	const announced = consumer.announced(Path.Pattern.subtree(Path.from("room")));

	// The initial state arrives first, named from the origin rather than the scope.
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("room/a"), kind: "start" });

	// Additions under the scope stream in; paths outside it are invisible.
	const b = publish(origin, Path.from("room/b"));
	publish(origin, Path.from("lobby/c"));
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("room/b"), kind: "start" });

	// Removals retract.
	b.close();
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("room/b"), kind: "end" });

	// The stream ends when the origin closes.
	origin.close();
	expect(await nextRoute(announced)).toBeUndefined();

	a.close();
});

test("hidden paths need an opt-in or a scope naming the dot segment", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const hidden = publish(origin, Path.from(".stats/node"));
	const nested = publish(origin, Path.from("room/.internal"));
	const visible = publish(origin, Path.from("room/catalog.pro"));

	expect([...consumer.broadcasts().peek().keys()]).toEqual([Path.from("room/catalog.pro")]);
	expect(consumer.broadcasts(undefined, { hidden: true }).peek().size).toBe(3);
	expect([...consumer.broadcasts(Path.Pattern.parse(".stats/**")).peek().keys()]).toEqual([Path.from(".stats/node")]);

	const plain = consumer.announced();
	expect(await nextRoute(plain)).toMatchObject({ prefix: Path.from("room/catalog.pro") });
	const opted = consumer.announced(Path.Pattern.parse("room/**"), { hidden: true });
	const seen = [(await nextRoute(opted))?.prefix, (await nextRoute(opted))?.prefix].sort();
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
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "start", route: Route.default });

	dispose();
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "end" });
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
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	dispose();
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("room"), kind: "end" });

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
	const dispose = serve(origin, path, provider(upstream), { hops: [], cost: { warm: 9n, cold: 9n } });

	const local = publish(origin, path);
	local.createTrack("local-track");

	// Local wins: the handle reaches the local track, not the remote one.
	const handle = await routed(consumer, path);
	const track = handle?.track("local-track").subscribe();
	expect(track).toBeDefined();
	track?.close();
	handle?.close();

	// One path, one announcement, even though both tables route it.
	const announced = consumer.announced();
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "start", route: Route.default });

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
	const table = wireOf(consumer).broadcasts.peek();
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
	const upstream = new BroadcastProducer();
	const slot = wireOf(origin).requests.peek()?.get(path);
	expect(slot).toBeDefined();
	expect(wireOf(origin).answer(path, upstream.consume())).toBeDefined();
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
	older.createTrack("chat");
	const keepOlder = older.consume();
	const disposeOlder = serve(origin, path, provider(older));

	const newer = new BroadcastProducer();
	const keepNewer = newer.consume();
	const disposeNewer = serve(origin, path, provider(newer));

	const announced = consumer.announced();
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "start" });

	// The newer session dies: consumers see a retract then the promoted fallback.
	disposeNewer();
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "end" });
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "start" });

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

	// Withdrawing pokes the requests map, which is what a standby serving loop sleeps on.
	const woken = wireOf(origin).requests.changed();
	withdraw?.();
	await woken;
	expect(request.active.peek()).toBeUndefined();

	// The slot is vacant again, so a standby answers.
	const third = new BroadcastProducer();
	expect(wireOf(origin).answer(path, third.consume())).toBeDefined();
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
	wireOf(origin).answer(path, upstream.consume());

	// An answered request is assumed present, not known live, so it is not availability: it
	// stays out of the table, and out of the announcements the table drives.
	expect(request.active.peek()).toBeDefined();
	expect(wireOf(origin).routes(path)).toBe(false);
	expect(wireOf(consumer).broadcasts.peek()?.has(path)).toBe(false);

	const announced = consumer.announced();
	publish(origin, Path.from("real"));
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("real"), kind: "start" });

	announced.close();
	request.close();
	origin.close();
});

test("a republish retracts then re-announces the path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	publish(origin, path);
	const announced = consumer.announced();
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "start" });

	// A new broadcast takes the path: consumers must let go of the superseded one.
	publish(origin, path);
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "end" });
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "start" });

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

	// Each request follows its own path's route, and nothing shared between them.
	expect(Math.max(...subscribers.values())).toBe(1);

	const slots = [...(wire.requests.peek()?.values() ?? [])];
	expect(slots.length).toBe(200);

	// Gated with no session attached and with full discovery; blind once one session cannot announce.
	expect(slots.some((slot) => wire.blind(slot))).toBe(false);
	const seeing = wire.attach(true);
	expect(slots.some((slot) => wire.blind(slot))).toBe(false);
	const silent = wire.attach(false);
	expect(slots.every((slot) => wire.blind(slot))).toBe(true);
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

test("a request is unroutable only when nothing can answer it", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("nowhere");

	// Nothing published, nothing attached, nothing coming: waiting here is futile.
	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();
	expect(request.unroutable.peek()).toBe(true);

	// A session attaches: now the path is merely unanswered.
	const detach = wireOf(origin).attach(false);
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
	const release = wireOf(origin).expect();

	const request = consumer.request(path);
	expect(request.unroutable.peek()).toBe(false);

	// A session comes and goes; the expectation still covers the gap.
	const detach = wireOf(origin).attach(true);
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

	const broadcast = publish(origin, path);
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

test("closing the origin makes an existing request unroutable", async () => {
	const origin = new Producer();
	const path = Path.from("doomed");

	const detach = wireOf(origin).attach(true);
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

test("createBroadcast is invisible to everyone until announce", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const broadcast = origin.createBroadcast(path);
	expect(wireOf(consumer).routes(path)).toBe(false);
	expect(wireOf(consumer).broadcasts.peek()?.has(path)).toBe(false);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(false);
	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();

	const announced = consumer.announced();
	broadcast.announce({ cost: 4n });
	expect(wireOf(consumer).routes(path)).toBe(true);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(true);
	expect(await nextRoute(announced)).toMatchObject({
		prefix: path,
		kind: "start",
		route: { hops: [], cost: { warm: 4n, cold: 4n } },
	});
	const first = request.active.peek();
	expect(first).toBeDefined();

	// Off the air for local consumers and peers alike.
	broadcast.unannounce();
	expect(wireOf(consumer).routes(path)).toBe(false);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(false);
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "end" });
	expect(request.active.peek()).toBeUndefined();

	// Back on the air through a fresh handle.
	broadcast.announce();
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "start" });
	const again = request.active.peek();
	expect(again).toBeDefined();
	expect(again).not.toBe(first);

	broadcast.close();
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "end" });
	request.close();
	announced.close();
	origin.close();
});

test("a request prefers a cheaper received route over an announced local broadcast", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const upstream = new BroadcastProducer();
	const dispose = serve(origin, path, provider(upstream), Route.normalize({ hops: [PEER], cost: 1n }));
	const local = origin.createBroadcast(path);
	local.announce({ cost: 3n });

	// The cheaper route is served on demand, so it resolves only once its handler answers.
	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();
	await settle();
	const remote = request.active.peek();
	expect(remote).toBeDefined();
	const announced = consumer.announced();
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, route: { hops: [PEER] } });

	// Re-priced below it, the local broadcast wins at once, and the remote front it replaced closes.
	local.announce({ cost: 0n });
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "end" });
	expect(await nextRoute(announced)).toMatchObject({ prefix: path, kind: "start", route: Route.default });
	expect(request.active.peek()).not.toBe(remote);
	expect(remote?.closed.peek()).not.toBeUndefined();

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
	const cheap = serve(origin, path, via("cheap"), Route.normalize({ hops: [PEER], cost: 1n }));
	const pricey = serve(origin, path, via("pricey"), Route.normalize({ hops: [PEER], cost: 10n }));
	const local = origin.createBroadcast(path);
	local.announce({ cost: 5n });

	expect(consumer.broadcasts().peek().get(path)).toEqual(Route.normalize({ hops: [PEER], cost: 1n }));
	const request = consumer.request(path);
	await settle();
	expect(request.active.peek()).toBeDefined();
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
	const handle = origin.dynamic(Path.from("live"));

	expect(await nextRoute(consumer.announced())).toMatchObject({ prefix: Path.from("live"), kind: "start" });

	const waiting = handle.requested().next();
	const request = consumer.request(Path.from("live/cam"));
	const { value: req } = await waiting;
	expect(req?.path).toBe(Path.from("live/cam"));

	const produced = new BroadcastProducer();
	produced.createTrack("video");
	req?.accept(produced);
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

test("a re-priced route is delivered as an update", async () => {
	const origin = new Producer();
	const handle = origin.dynamic(Path.from("live"));
	const announced = origin.consume().announced();
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("live"), kind: "start" });
	handle.update({ cost: 5n });
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("live"), kind: "update" });
	handle.close();
	expect(await nextRoute(announced)).toMatchObject({ prefix: Path.from("live"), kind: "end" });
	announced.close();
	origin.close();
});

test("an accepted dynamic broadcast is retired when it closes", async () => {
	const origin = new Producer();
	const handle = origin.dynamic(Path.from("live"));
	const path = Path.from("live/cam");
	const request = origin.request(path);
	const it = handle.requested();

	const first = await it.next();
	const produced = new BroadcastProducer();
	produced.createTrack("video");
	first.value?.accept(produced);
	await settle();
	expect(request.active.peek()?.track("video").subscribe()).toBeDefined();

	produced.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();

	const second = await it.next();
	expect(second.value?.path).toBe(path);
	const replacement = new BroadcastProducer();
	replacement.createTrack("video");
	second.value?.accept(replacement);
	await settle();
	expect(request.active.peek()?.track("video").subscribe()).toBeDefined();

	request.close();
	handle.close();
	replacement.close();
	origin.close();
});

test("reject surfaces the error from demand", async () => {
	const origin = new Producer();
	const handle = origin.dynamic(Path.from("live"));
	const consumer = origin.consume();
	const it = handle.requested();
	const pending = wireOf(consumer).demand(Path.from("live/cam"));
	const { value: req } = await it.next();
	const err = new StreamError(StreamCode.NoCapacity, { message: "full" });
	req?.reject(err);
	await expect(pending).rejects.toBe(err);

	handle.close();
	origin.close();
});

test("a peer is offered and served the cheapest originated route", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const prefix = Path.from("live");
	const cheap = origin.dynamic(prefix, { cost: 1n });
	const pricey = origin.dynamic(prefix, { cost: 10n });

	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(Route.normalize({ cost: 1n }));
	const pending = wireOf(consumer).demand(Path.from("live/cam"));
	const { value: req } = await cheap.requested().next();
	const upstream = new BroadcastProducer();
	req?.accept(upstream.consume());
	expect(await pending).toBeDefined();

	// An exact-path local broadcast competes with them on cost too.
	const local = origin.createBroadcast(prefix);
	local.announce({ cost: 5n });
	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(Route.normalize({ cost: 1n }));
	local.announce({ cost: 0n });
	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(Route.normalize({ cost: 0n }));

	local.close();
	upstream.close();
	pricey.close();
	cheap.close();
	origin.close();
});

test("a shared reject still surfaces from demand", async () => {
	const origin = new Producer();
	const handle = origin.dynamic(Path.from("live"));
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
	const handle = origin.dynamic(Path.from("live"));
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
	second?.accept(produced);
	await expect(pending).resolves.toBeDefined();

	handle.close();
	produced.close();
	origin.close();
});

test("advancing requested without settling rejects the previous request", async () => {
	const origin = new Producer();
	const handle = origin.dynamic(Path.from("live"));
	const consumer = origin.consume();
	const it = handle.requested();

	const firstDemand = wireOf(consumer).demand(Path.from("live/cam"));
	const first = await it.next();
	expect(first.value?.path).toBe(Path.from("live/cam"));

	const secondDemand = wireOf(consumer).demand(Path.from("live/other"));
	const second = await it.next();
	expect(second.value?.path).toBe(Path.from("live/other"));

	await expect(firstDemand).rejects.toMatchObject({ code: StreamCode.NoCapacity });
	const produced = new BroadcastProducer();
	second.value?.accept(produced);
	await expect(secondDemand).resolves.toBeDefined();

	handle.close();
	produced.close();
	origin.close();
});

test("a rejected request is not asked of the same route again", async () => {
	const origin = new Producer();
	const handle = origin.dynamic(Path.from("live"));
	const it = handle.requested();

	const request = origin.request(Path.from("live/cam"));
	const { value: first } = await it.next();
	expect(first?.path).toBe(Path.from("live/cam"));
	first?.reject(new Error("unserved"));
	await settle();

	// A refusal is authoritative: the path resolves to nothing rather than landing back on
	// the queue, so the handler is asked once and the request reads as unroutable.
	expect(request.active.peek()).toBeUndefined();
	expect(request.unroutable.peek()).toBe(true);
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

// A relay migration lands the replacement session's route next to the draining one's. The
// request must hand over to it, not drop to nothing while the new session answers.
test("an outranked route keeps serving until its replacement answers", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("migrating");

	const older = new BroadcastProducer();
	const keepOlder = older.consume();
	const disposeOlder = serve(origin, path, () => keepOlder.clone());

	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	let gaps = 0;
	const stop = request.active.subscribe((active) => {
		if (active === undefined) gaps++;
	});

	// The newer route wins the tie but has not answered yet.
	const newer = wireOf(origin).receive(path);
	const asked = newer.requested().next();
	await settle();
	expect(request.active.peek()).toBe(first);

	// Once it answers, the request swaps straight across.
	const replacement = new BroadcastProducer();
	const { value: req } = await asked;
	req?.accept(replacement.consume());
	await settle();
	expect(request.active.peek()).toBeDefined();
	expect(request.active.peek()).not.toBe(first);
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

test("a refusal is terminal, never falling through to a broader route", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const narrow = origin.dynamic(Path.from("live"));
	const wide = origin.dynamic(Path.from(""));
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
	expect(request.unroutable.peek()).toBe(true);
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
	const disposeWide = serve(origin, Path.from(""), provider(served));

	const request = consumer.request(Path.from("live/cam"));
	await settle();
	const before = request.active.peek();
	expect(before).toBeDefined();

	// The narrower route is asked while the broad one keeps serving, then says no. A
	// costlier sibling at the same prefix could serve, but is never asked.
	const narrow = origin.dynamic(Path.from("live"), { cost: 1n });
	const sibling = origin.dynamic(Path.from("live"), { cost: 2n });
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
	const wide = origin.dynamic(Path.from(""));
	const wideRequests = wide.requested();

	const request = consumer.request(Path.from("live/cam"));
	const { value: stale } = await wideRequests.next();

	// A narrower route takes over the pending request before the broad one answers.
	const narrow = origin.dynamic(Path.from("live"));
	const { value: req } = await narrow.requested().next();
	stale?.reject(new Error("unserved"));
	await settle();
	expect(request.closed.peek()).toBeUndefined();

	const produced = new BroadcastProducer();
	req?.accept(produced);
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
	const wide = origin.dynamic(Path.from("live"));
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

test("close rejects queued requests with NoCapacity", async () => {
	const origin = new Producer();
	const handle = origin.dynamic(Path.from("live"));
	const waiting = handle.requested().next();
	const request = origin.request(Path.from("live/cam"));
	const { value: req } = await waiting;
	expect(req).toBeDefined();

	handle.close();
	await settle();

	expect((await handle.requested().next()).done).toBe(true);
	expect(request.active.peek()).toBeUndefined();
	req?.accept(new BroadcastProducer());
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(Number(new StreamError(StreamCode.NoCapacity).code)).toBe(0x30);

	request.close();
	origin.close();
});

test("announced filters by arbitrary patterns and reports captures", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const broadcast = publish(origin, Path.from("room/alice"));
	const announced = consumer.announced(Path.Pattern.parse("room/*"));

	const update = await nextRoute(announced);
	expect(update?.prefix).toBe(Path.from("room/alice"));
	expect(update?.captures?.map((capture) => capture.text)).toEqual(["alice"]);

	announced.close();
	broadcast.close();
	origin.close();
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
	expect(origin.broadcasts().peek().has(Path.from("tenant/room/alice"))).toBe(true);
	expect([...scoped.broadcasts().peek().keys()]).toEqual([Path.from("room/alice")]);
	const request = scoped.consume().request(Path.from("room/alice"));
	expect(request.path).toBe(Path.from("room/alice"));
	expect(request.active.peek()).toBeDefined();
	expect(() => scoped.createBroadcast(Path.from("other"))).toThrow("outside the origin scope");
	expect(() => scoped.request(Path.from("room/alice/deep"))).toThrow("outside the origin scope");
	expect(() => scoped.dynamic(Path.from("other"))).toThrow("outside the origin scope");
	expect(() => scoped.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("other/**")]))).toThrow(
		"do not overlap",
	);
	expect(() => origin.scope(Path.empty(), new Path.Patterns())).toThrow("do not overlap");

	const nested = scoped.scope(Path.from("room"), new Path.Patterns([Path.Pattern.all()]));
	expect([...nested.broadcasts().peek().keys()]).toEqual([Path.from("alice")]);
	expect(() => nested.createBroadcast(Path.from("alice/deep"))).toThrow("outside the origin scope");
	const announced = nested.announced();
	expect(await announced.next()).toMatchObject({ kind: "start", prefix: Path.from("alice") });
	announced.close();
	request.close();
	broadcast.close();
	origin.close();
});

test("a scoped dynamic only serves allowed paths and presents relative requests", async () => {
	const origin = new Producer();
	const scoped = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.parse("room/*/chat")]));
	const dynamic = scoped.dynamic(Path.from("room"));
	expect(dynamic.prefix).toBe(Path.from("room"));
	const denied = origin.request(Path.from("tenant/room/alice/video"));
	expect(denied.unroutable.peek()).toBe(true);
	const request = origin.request(Path.from("tenant/room/alice/chat"));
	const requests = dynamic.requested();
	const pending = await requests.next();
	expect(pending.value?.path).toBe(Path.from("room/alice/chat"));
	const broadcast = new BroadcastProducer();
	pending.value?.accept(broadcast);
	expect(request.active.peek()).toBeDefined();
	dynamic.update({ cost: 2n });
	expect(origin.broadcasts().peek().get(Path.from("tenant/room"))?.cost.warm).toBe(2n);
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

test("a scoped dynamic is announced only to readers its scope can serve", () => {
	const origin = new Producer();
	const dynamic = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("*/chat")])).dynamic(Path.empty());
	const chat = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/chat")]));
	const video = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/video")]));
	expect([...chat.broadcasts().peek().keys()]).toEqual([Path.empty()]);
	expect([...video.broadcasts().peek().keys()]).toEqual([]);
	expect([...(wireOf(chat.consume()).advertised.peek()?.keys() ?? [])]).toEqual([Path.empty()]);
	expect([...(wireOf(video.consume()).advertised.peek()?.keys() ?? [])]).toEqual([]);
	dynamic.close();
	origin.close();
});

test("a rooted reader presents the most specific covering route", () => {
	const origin = new Producer();
	const narrow = origin.dynamic(Path.from("room/alice"), { cost: 9n });
	const broad = origin.dynamic(Path.from("room"), { cost: 1n });
	const rooted = origin.scope(Path.from("room/alice"), new Path.Patterns([Path.Pattern.all()]));
	expect(rooted.broadcasts().peek().get(Path.empty())?.cost.warm).toBe(9n);
	expect(wireOf(rooted.consume()).advertised.peek()?.get(Path.empty())?.[0]?.route.cost.warm).toBe(9n);
	broad.close();
	narrow.close();
	origin.close();
});

test("a scoped reader picks the best route its scope can see at a prefix", async () => {
	const origin = new Producer();
	const scoped = (pattern: string) => origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse(pattern)]));
	const chat = scoped("*/chat").dynamic(Path.empty(), { cost: 1n });
	const video = scoped("*/video").dynamic(Path.empty(), { cost: 5n });
	const reader = scoped("room/video");

	expect(reader.broadcasts().peek().get(Path.empty())?.cost.warm).toBe(5n);
	expect(origin.broadcasts(Path.Pattern.parse("room/video")).peek().get(Path.empty())?.cost.warm).toBe(5n);
	const announced = reader.announced();
	expect((await nextRoute(announced))?.route.cost.warm).toBe(5n);
	announced.close();

	// A session publishing the scoped view offers the video route too.
	const offered = [...(wireOf(reader.consume()).advertised.peek()?.get(Path.empty()) ?? [])];
	expect(offered.map((advert) => advert.route.cost.warm)).toEqual([5n]);

	video.close();
	chat.close();
	origin.close();
});

test("a scoped reader sees a local broadcast that only loses to a route outside its scope", () => {
	const origin = new Producer();
	const chat = origin
		.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/*/chat")]))
		.dynamic(Path.from("room/alice"), { cost: 0n });
	const local = origin.createBroadcast(Path.from("room/alice"));
	local.announce({ cost: 5n });
	const reader = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/*")]));

	expect(reader.broadcasts().peek().get(Path.from("room/alice"))?.cost.warm).toBe(5n);
	// Unscoped, the cheaper route still wins the prefix.
	expect(origin.broadcasts().peek().get(Path.from("room/alice"))?.cost.warm).toBe(0n);

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
	const dynamic = origin.dynamic(Path.from("live"));
	const request = origin.request(Path.from("live/bob"));
	const pending = await dynamic.requested().next();
	const served = new BroadcastProducer();
	pending.value?.accept(served);
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

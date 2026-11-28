import { expect, spyOn, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { getter, Once, race, Signal } from "@moq/signals";
import fc from "fast-check";
import { announcedView } from "./announced.test-util";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { askedFirst } from "./claim.test-util.ts";
import { defined } from "./defined.test-util.ts";
import { StreamCode, StreamError } from "./error.ts";
import { type Cost, toHop, WireRoute } from "./hop.ts";
import * as Identity from "./identity.ts";
import { arrival, compareRoutes, hooks, mostSpecific, presented, spreadHash, superseded } from "./internal.ts";
import * as Mint from "./mint.ts";
import { SessionBroadcast, textFrame } from "./mock.ts";
import type { Consumer, Table } from "./origin.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";
import { readsFrom } from "./resume.test-util";
import { Timescale, Timestamp } from "./time.ts";
import { overrideBroadcastWire, wireOf } from "./wire.ts";

function publish(origin: Producer, path: Path.Valid) {
	const broadcastRoute = origin.route(path);
	const broadcast = broadcastRoute.broadcast;
	return broadcast;
}

/** A peer's hop, for a route that is not local. */
const PEER = toHop(10n);
// One publisher identity, so a local broadcast and a remote route compete on cost alone.
const IDENTITY = Identity.parse("01900000-0000-7000-8000-000000000001");

/** Land a received prefix, served from `consume`, the way a session does. */
function serve(
	origin: Producer,
	prefix: Path.Valid,
	consume: () => BroadcastConsumer,
	route: WireRoute = WireRoute.default,
) {
	const handle = wireOf(origin).receive(prefix, route);
	void (async () => {
		try {
			for await (const request of handle.requested()) {
				request.serve(consume());
			}
		} catch {
			handle.close();
		}
	})();
	return () => handle.close();
}

async function settle() {
	await yieldNow();
}

for (const via of ["direct", "request", "follow"] as const) {
	test.each([new Error("publisher aborted"), new StreamError(StreamCode.FrameTooLarge)])(
		`a ${via} group's pending frame read receives its track's abort: %s`,
		async (cause) => {
			const origin = new Producer();
			const path = Path.from("live");
			const broadcast = publish(origin, path);
			const track = defined(broadcast.createTrack("video", { maxAge: null }));
			const request = via === "direct" ? undefined : origin.consume()[via](path);
			await settle();
			const consumed = request ? request.active.peek() : broadcast.consume();
			if (!consumed) throw new Error("expected a broadcast");
			const sub = consumed.track("video").subscribe();
			const history = hooks.createEmpty(track, 0n);
			history.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.unscaled(history.start) });
			history.finish();
			const finished = await sub.recvGroup();
			const live = hooks.createEmpty(track, 1n);
			live.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.unscaled(live.start) });
			const group = await sub.recvGroup();
			if (!finished || !group) throw new Error("expected both groups");
			try {
				expect((await group.readFrame())?.payload).toEqual(new Uint8Array([2]));
				let outcome: unknown = "pending";
				const read = group.readFrame().then(
					(frame) => (outcome = frame),
					(error) => (outcome = error),
				);
				await settle();
				expect(outcome).toBe("pending");
				track.abort(cause);
				await settle();
				expect(outcome).toBe(cause);
				await read;
				expect((await finished.readFrame())?.payload).toEqual(new Uint8Array([1]));
				expect(await finished.readFrame()).toBeUndefined();
				await expect(sub.finished()).rejects.toBe(cause);
				await expect(sub.recvGroup()).rejects.toBe(cause);
			} finally {
				sub.close();
				request?.close();
				if (!request) consumed.close();
				origin.close();
			}
		},
	);
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
	// The handle is the request's logical front, which ends with the request, so the request
	// stays open for the test.
	return request.active.peek()?.clone();
}

/** A stand-in for a session announcing a route: serves any path from `producer`. */
function provider(producer: BroadcastProducer) {
	return () => producer.consume();
}

test("a borrowed table exposes dynamic serving and a live scoped broadcast map", async () => {
	const origin = new Producer();
	const table: Table = origin;
	const scope = Path.Pattern.parse("room/**");
	const live = await announcedView(origin, scope);
	const changes: ReadonlyMap<Path.Valid, WireRoute>[] = [];
	const stop = live.subscribe((value) => changes.push(value));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().size).toBe(0);

	const other = publish(origin, Path.from("other"));
	const localPath = Path.from("room/alice");
	const localRoute = table.route(localPath, { hidden: true, identity: { id: IDENTITY, epoch: 0n }, cost: 4n });
	const local = localRoute.broadcast;
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().has(localPath)).toBe(false);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().has(Path.from("other"))).toBe(false);

	localRoute.announce();
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(localPath)).toEqual(WireRoute.normalize({ identity: IDENTITY, cost: 4n }));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(changes.some((value) => value.get(localPath)?.cost === 4n)).toBe(true);

	const prefix = Path.from("room");
	const dynamic = table.claim(prefix, { cost: 2n });
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(prefix)).toEqual(WireRoute.normalize({ ...wireOf(dynamic).info, cost: 2n }));
	dynamic.setCost(3n);
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(prefix)).toEqual(WireRoute.normalize({ ...wireOf(dynamic).info, cost: 3n }));

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
	const remote = wireOf(origin).receive(path, { identity: IDENTITY, hops: [PEER], cost: 5n });
	const live = await announcedView(consumer);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 5n }));

	// Unannounced, it competes for nothing.
	const localRoute = origin.route(path, { hidden: true, identity: { id: IDENTITY, epoch: 0n }, cost: 9n });
	const local = localRoute.broadcast;
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 5n }));

	// Cheaper wins; a tie falls to the local broadcast.
	localRoute.announce();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 5n }));
	localRoute.setCost(5n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(WireRoute.normalize({ identity: IDENTITY, cost: 5n }));

	local.close();
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toEqual(WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 5n }));

	remote.close();
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().size).toBe(0);
	origin.close();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().size).toBe(0);
});

// A covering reader sees a broadcast at a path its scope only overlaps, as Rust's cursor does: it
// ranks with the routes there, so the cheaper broadcast is listed over a costlier route.
test("a covering listing shows a broadcast at a path its scope only overlaps", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const remote = wireOf(origin).receive(path, { cost: 5n });
	const local = publish(origin, path);
	const scope = Path.Pattern.parse("room/*");
	const live = await announcedView(origin, scope, { covering: true });

	await new Promise<void>((resolve) => setImmediate(resolve));

	expect((await announcedView(origin)).peek().get(path)).toMatchObject(WireRoute.default);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(live.peek().get(path)).toMatchObject(WireRoute.default);

	// Withdrawn, the route serving the subtree is listed again.
	local.close();
	await settle();
	expect(live.peek().get(path)).toMatchObject({ cost: 5n });

	remote.close();
	origin.close();
});

// A broadcast beneath a broader claim shadows it for the paths beneath the broadcast, so a covering
// reader of those paths is shown both.
test("a covering listing shows a broadcast that shadows a broader claim", async () => {
	const origin = new Producer();
	const broad = origin.claim(Path.empty());
	const broadcast = origin.route(Path.from("room"));
	const live = await announcedView(origin, Path.Pattern.parse("room/*"), { covering: true });
	await settle();
	expect([...live.peek().keys()].sort()).toEqual([Path.empty(), Path.from("room")]);
	expect(live.peek().get(Path.from("room"))?.identity).toBe(broadcast.identity);
	await live.close();
	broadcast.close();
	broad.close();
	origin.close();
});

test("a scope cut at an escaped slash discovers through the nearest valid ancestor", async () => {
	const origin = new Producer();
	const path = Path.from("room/a\\/b");
	const broadcast = publish(origin, path);
	const live = await announcedView(origin, Path.Pattern.parse("room/a\\/*"));
	await settle();

	expect([...live.peek().keys()]).toEqual([path]);

	broadcast.close();
	origin.close();
});

test("a published broadcast resolves by path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const path = Path.from("room");
	expect(wireOf(consumer).routes(path)).toBe(false);

	const broadcast = publish(origin, path);
	defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));

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

	const publication = origin.route(path, { hidden: true });
	const first = publication.broadcast;
	const track = defined(first.createTrack("video", { timescale: Timescale.MILLI }));
	publication.announce();
	const mine = await routed(consumer, path);
	const subscriber = mine?.track("video").subscribe();
	if (!subscriber) throw new Error("expected a subscriber");

	const replacement = origin.route(path);
	// The origin closes the old producer itself, whoever still holds a handle on it.
	expect(first.closed.peek()).toBeNull();
	// Its stale publication never displaces or retracts the replacement.
	expect(() => publication.announce()).toThrow("broadcast is closed");
	publication.withdraw();
	const current = await routed(consumer, path);
	expect(readsFrom(current, replacement.broadcast)).toBe(true);
	current?.close();

	// A track already subscribed is not the broadcast: it keeps serving until it ends.
	// The frame's time starts its group.
	track.writeFrame(textFrame("late", Timestamp.fromMillis(5)));
	expect((await subscriber.recvGroup())?.start).toBe(5n);

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

	const announced = consumer.routes(Path.Pattern.subtree(Path.from("room")));

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

	const plain = consumer.routes();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(await plain.next()).toMatchObject({ prefix: Path.from("room/catalog.pro") });
	const opted = consumer.routes(Path.Pattern.parse("room/**"), { hidden: true });
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
	const announced = consumer.routes();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start", route: WireRoute.default });

	dispose();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "offline" });
	expect(wireOf(consumer).routes(path)).toBe(false);

	announced.close();
	upstream.close();
	origin.close();
});

test("a received route its announcer ends ends, and one its session loses goes offline", async () => {
	const origin = new Producer();
	const announced = origin.consume().routes();

	const ended = wireOf(origin).receive(Path.from("ended"), WireRoute.default);
	const lost = wireOf(origin).receive(Path.from("lost"), WireRoute.default);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("ended"), kind: "start" });
	expect(await announced.next()).toMatchObject({ prefix: Path.from("lost"), kind: "start" });

	wireOf(ended).end();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("ended"), kind: "end" });
	lost.close();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("lost"), kind: "offline" });

	announced.close();
	origin.close();
});

test("a route received through a scoped handle goes offline when the handle closes", async () => {
	const origin = new Producer();
	const announced = origin.consume().routes();

	const scoped = origin.scope(Path.empty(), Path.Prefixes.all());
	const route = wireOf(scoped).receive(Path.from("room"), WireRoute.default);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	scoped.close();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "offline" });
	route.close();

	announced.close();
	origin.close();
});

test("announced keeps a broader covering route at its prefix", async () => {
	const origin = new Producer();
	const consumer = origin.consume();

	const upstream = new BroadcastProducer();
	const dispose = serve(origin, Path.from("room"), provider(upstream));

	// The scope filters the route without changing the prefix it claims.
	const scope = Path.Pattern.subtree(Path.from("room/alice"));
	const announced = consumer.routes(scope, { covering: true });
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	dispose();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "offline" });

	announced.close();
	upstream.close();
	origin.close();
});

test("a local publish shadows a remote entry", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const upstream = new BroadcastProducer();
	defined(upstream.createTrack("remote-track", { timescale: Timescale.MILLI }));
	const dispose = serve(origin, path, provider(upstream), { hops: [], cost: 9n });

	const local = publish(origin, path);
	defined(local.createTrack("local-track", { timescale: Timescale.MILLI }));

	// Local wins: the handle reaches the local track, not the remote one.
	const handle = await routed(consumer, path);
	const track = handle?.track("local-track").subscribe();
	expect(track).toBeDefined();
	track?.close();
	handle?.close();

	// One path, one announcement, even though both tables route it.
	const announced = consumer.routes();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start", route: WireRoute.default });

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
	defined(older.createTrack("chat", { timescale: Timescale.MILLI }));
	const keepOlder = older.consume();
	const disposeOlder = serve(origin, path, provider(older));

	const newer = new BroadcastProducer();
	const keepNewer = newer.consume();
	const disposeNewer = serve(origin, path, provider(newer));

	const announced = consumer.routes();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });

	// The newer session dies: without an identity the promoted fallback is another source.
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
	// request it answered ends: the next answer is another session, so another identity.
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

	const announced = consumer.routes();
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
	const announced = consumer.routes();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });

	// A new broadcast takes the path: consumers must let go of the replaced one.
	publish(origin, path);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart" });

	announced.close();
	origin.close();
});

test("an identical reconnect restarts without an identity and is quiet with one", async () => {
	for (const identity of [undefined, IDENTITY]) {
		const origin = new Producer();
		const consumer = origin.consume();
		const path = Path.from("room");
		const announced = consumer.routes();
		const route = WireRoute.normalize({ identity, hops: [PEER] });

		const old = wireOf(origin).receive(path, route);
		expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });

		// The fresh session wins the tie: another source unless the identity says otherwise.
		const fresh = wireOf(origin).receive(path, route);
		if (identity === undefined) expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart" });

		// Retracting the stale twin is quiet, and a re-price of the winner is an update.
		old.close();
		wireOf(fresh).update({ ...route, cost: 5n });
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

test("followers share no subscription and follow discovery", () => {
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
		for (let i = 0; i < 200; i++) requests.push(origin.follow(Path.from(`user${i}`)));
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

test("a hidden route is invisible to everyone until announce", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");

	const broadcastRoute = origin.route(path, { hidden: true, cost: 4n });
	const broadcast = broadcastRoute.broadcast;
	expect(wireOf(consumer).routes(path)).toBe(false);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(false);
	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();

	const announced = consumer.routes();
	expect(broadcastRoute.announced).toBe(false);
	broadcastRoute.announce();
	expect(broadcastRoute.announced).toBe(true);
	expect(wireOf(consumer).routes(path)).toBe(true);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(true);
	expect(await announced.next()).toMatchObject({
		prefix: path,
		kind: "start",
		route: { identity: broadcastRoute.identity, hops: [], cost: 4n },
	});
	const first = request.active.peek();
	expect(first?.identity).toBe(broadcastRoute.identity);

	// Off the air for local consumers and peers alike, which ends the request on it.
	broadcastRoute.withdraw();
	expect(wireOf(consumer).routes(path)).toBe(false);
	expect(wireOf(consumer).advertised.peek()?.has(path)).toBe(false);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "end" });
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeInstanceOf(Error);

	// Back on the air for a fresh request, through a fresh handle, continuing the same identity.
	const identity = broadcastRoute.identity;
	broadcastRoute.announce();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start", route: { identity } });
	const fresh = consumer.request(path);
	const again = fresh.active.peek();
	expect(again).toBeDefined();
	expect(again).not.toBe(first);
	expect(again?.identity).toBe(identity);

	// Closing the broadcast retracts the route at once, for good.
	broadcast.close();
	expect(broadcastRoute.announced).toBe(false);
	expect(wireOf(consumer).routes(path)).toBe(false);
	expect(() => broadcastRoute.announce()).toThrow("broadcast is closed");
	expect(() => broadcastRoute.setCost(1n)).toThrow("broadcast is closed");
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "end" });
	fresh.close();
	request.close();
	announced.close();
	origin.close();
});

test("closing the origin ends a hidden or withdrawn prefix route's requests", async () => {
	const origin = new Producer();
	const hidden = origin.claim(Path.from("hidden"), { hidden: true });
	const withdrawn = origin.claim(Path.from("withdrawn"));
	withdrawn.withdraw();
	const pending = [hidden.requested().next(), withdrawn.requested().next()];

	origin.close();
	for (const next of await Promise.all(pending)) expect(next.done).toBe(true);
});

test("a prefix route refuses to announce or re-price once its origin closed, like an exact one", () => {
	const origin = new Producer();
	const prefix = origin.claim(Path.from("live"));
	const exact = origin.route(Path.from("room"));
	origin.close();
	expect(prefix.announced).toBe(false);
	expect(() => prefix.announce()).toThrow("origin is closed");
	expect(() => prefix.setCost(1n)).toThrow("origin is closed");
	expect(() => exact.announce()).toThrow();
	expect(() => exact.setCost(1n)).toThrow();
});

test("a claim can start hidden, withdraw and come back while it keeps yielding requests", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const prefix = Path.from("jobs");
	const path = Path.from("jobs/a");
	const route = origin.claim(prefix, { hidden: true, cost: 3n });
	expect(route.announced).toBe(false);
	expect(wireOf(consumer).routes(path)).toBe(false);
	const requests = route.requested();

	route.announce();
	expect(route.announced).toBe(true);
	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(
		WireRoute.normalize({ identity: route.identity, cost: 3n }),
	);
	const first = consumer.request(path);
	const asked = (await requests.next()).value;
	if (!asked) throw new Error("expected a request");

	// Withdrawn, the route answers nothing: the handler's late answer is dropped, and the request
	// waits for a route, as it would between reconnects.
	route.withdraw();
	expect(route.announced).toBe(false);
	expect(wireOf(consumer).routes(path)).toBe(false);
	const dropped = new BroadcastProducer();
	asked.serve(dropped);
	await settle();
	expect(first.active.peek()).toBeUndefined();

	// Back under the same identity, the same iterator yields the request again.
	const identity = route.identity;
	route.announce();
	expect(route.identity).toBe(identity);
	const second = consumer.request(path);
	const next = (await requests.next()).value;
	if (!next) throw new Error("expected another request");
	const source = new BroadcastProducer();
	next.serve(source);
	await settle();
	for (const request of [first, second]) expect(request.active.peek()?.identity).toBe(identity);

	first.close();
	second.close();
	dropped.close();
	route.close();
	expect((await requests.next()).done).toBe(true);
	expect(() => route.announce()).toThrow("claim is closed");
	source.close();
	origin.close();
});

test("closing an exact route closes its broadcast", () => {
	const origin = new Producer();
	const route = origin.route(Path.from("room"));
	route.close();
	expect(route.broadcast.closed.peek()).toBeNull();
	origin.close();
});

test("a request prefers a cheaper received route over an announced local broadcast", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const upstream = new BroadcastProducer();
	const dispose = serve(
		origin,
		path,
		provider(upstream),
		WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 1n }),
	);
	const localRoute = origin.route(path, { identity: { id: IDENTITY, epoch: 0n }, cost: 3n });
	const local = localRoute.broadcast;

	// The cheaper route is served on demand, so it resolves only once its handler answers.
	const request = consumer.request(path);
	expect(request.active.peek()).toBeUndefined();
	await settle();
	const remote = request.active.peek();
	expect(remote).toBeDefined();
	const announced = consumer.routes();
	expect(await announced.next()).toMatchObject({ prefix: path, route: { hops: [PEER] } });

	// Re-priced below it, the local broadcast of the same identity wins at once: the request's
	// broadcast stays, and its tracks read the local one.
	localRoute.setCost(0n);
	expect(await announced.next()).toMatchObject({
		prefix: path,
		kind: "update",
		route: { identity: IDENTITY, hops: [] },
	});
	expect(request.active.peek()).toBe(remote);
	expect(readsFrom(remote, local)).toBe(true);

	announced.close();
	request.close();
	local.close();
	dispose();
	upstream.close();
	origin.close();
});

test("an announced local broadcast shadows a broader claim shown at its path whatever its epoch", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const upstream = new BroadcastProducer();
	const dispose = serve(
		origin,
		path,
		provider(upstream),
		WireRoute.normalize({ identity: IDENTITY, epoch: 1n, hops: [PEER], cost: 1n, above: 1n }),
	);
	const localRoute = origin.route(path, { identity: { id: IDENTITY, epoch: 0n }, cost: 3n });
	const local = localRoute.broadcast;

	const request = consumer.request(path);
	await settle();
	const active = request.active.peek();
	expect(active).toBeDefined();
	expect(readsFrom(active as BroadcastConsumer, local)).toBe(true);

	request.close();
	local.close();
	dispose();
	upstream.close();
	origin.close();
});

test("a broader claim shown at a path never supersedes the local broadcast against an exact route", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const other = Identity.parse("01900000-0000-7000-8000-000000000002");
	const exact = new BroadcastProducer();
	const broad = new BroadcastProducer();
	const disposeExact = serve(
		origin,
		path,
		provider(exact),
		WireRoute.normalize({ identity: other, hops: [PEER], cost: 5n }),
	);
	const disposeBroad = serve(
		origin,
		path,
		provider(broad),
		WireRoute.normalize({ identity: IDENTITY, epoch: 1n, hops: [PEER], cost: 1n, above: 1n }),
	);
	const localRoute = origin.route(path, { identity: { id: IDENTITY, epoch: 0n }, cost: 3n });
	const local = localRoute.broadcast;

	const request = consumer.request(path);
	await settle();
	const active = request.active.peek();
	expect(active).toBeDefined();
	expect(readsFrom(active as BroadcastConsumer, local)).toBe(true);

	request.close();
	local.close();
	disposeExact();
	disposeBroad();
	exact.close();
	broad.close();
	origin.close();
});

test("an announced local broadcast shadows a cheaper broader claim shown at its path", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const upstream = new BroadcastProducer();
	const dispose = serve(
		origin,
		path,
		provider(upstream),
		WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 1n, above: 1n }),
	);
	const localRoute = origin.route(path, { identity: { id: IDENTITY, epoch: 0n }, cost: 3n });
	const local = localRoute.broadcast;

	const request = consumer.request(path);
	await settle();
	const active = request.active.peek();
	expect(active).toBeDefined();
	expect(readsFrom(active as BroadcastConsumer, local)).toBe(true);

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
	const cheap = serve(
		origin,
		path,
		via("cheap"),
		WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 1n }),
	);
	const pricey = serve(
		origin,
		path,
		via("pricey"),
		WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 10n }),
	);
	const localRoute = origin.route(path, { identity: { id: IDENTITY, epoch: 0n }, cost: 5n });
	const local = localRoute.broadcast;

	await new Promise<void>((resolve) => setImmediate(resolve));

	expect((await announcedView(consumer)).peek().get(path)).toEqual(
		WireRoute.normalize({ identity: IDENTITY, hops: [PEER], cost: 1n }),
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
	const handle = origin.claim(Path.from("live"));

	expect(await consumer.routes().next()).toMatchObject({ prefix: Path.from("live"), kind: "start" });

	const waiting = handle.requested().next();
	const request = consumer.request(Path.from("live/cam"));
	const { value: req } = await waiting;
	expect(req?.path).toBe(Path.from("live/cam"));

	const produced = new BroadcastProducer();
	defined(produced.createTrack("video", { timescale: Timescale.MILLI }));
	req.serve(produced);
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

test("an originated route serves held content a session delivered", async () => {
	const origin = new Producer();
	const handle = origin.claim(Path.from("live"));
	const it = handle.requested();
	const pending = wireOf(origin.consume()).demand(Path.from("live/cam"));
	const { value: req } = await it.next();
	if (!req) throw new Error("expected request");

	// What lite and IETF do when a session hands back a consumed broadcast.
	const foreign = new BroadcastProducer().consume();
	overrideBroadcastWire(foreign, {});
	req.serve(foreign);
	expect(await pending).toBeDefined();

	handle.close();
	foreign.close();
	origin.close();
});

test("a received route still serves a broadcast a session delivered", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const upstream = new BroadcastProducer();
	defined(upstream.createTrack("video"));
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
	const handle = origin.claim(Path.from("live"));
	const announced = origin.consume().routes();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("live"), kind: "start" });
	handle.setCost(5n);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("live"), kind: "update" });
	handle.close();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("live"), kind: "end" });
	announced.close();
	origin.close();
});

test("an accepted dynamic broadcast is retired when it closes, and its handle resumes", async () => {
	const origin = new Producer();
	const handle = origin.claim(Path.from("live"));
	const path = Path.from("live/cam");
	const request = origin.request(path);
	const it = handle.requested();

	const first = await it.next();
	const produced = new BroadcastProducer();
	defined(produced.createTrack("video", { timescale: Timescale.MILLI }));
	first.value.serve(produced);
	await settle();
	const held = request.active.peek();
	expect(held?.track("video").subscribe()).toBeDefined();

	// The claim is asked again, and its next answer serves the same identity, so the handle
	// the request holds continues on it.
	produced.close();
	await settle();
	const second = await it.next();
	expect(second.value?.path).toBe(path);
	const replacement = new BroadcastProducer();
	defined(replacement.createTrack("video", { timescale: Timescale.MILLI }));
	second.value.serve(replacement);
	await settle();
	expect(request.active.peek()).toBe(held);
	expect(readsFrom(held, replacement)).toBe(true);

	request.close();
	handle.close();
	replacement.close();
	origin.close();
});

test("reject surfaces the error from demand", async () => {
	const origin = new Producer();
	const handle = origin.claim(Path.from("live"));
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
	const cheap = origin.claim(prefix, { identity: { id: IDENTITY, epoch: 0n }, cost: 1n });
	const pricey = origin.claim(prefix, { identity: { id: IDENTITY, epoch: 0n }, cost: 10n });

	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(
		WireRoute.normalize({ identity: IDENTITY, cost: 1n }),
	);
	const pending = wireOf(consumer).demand(Path.from("live/cam"));
	const { value: req } = await cheap.requested().next();
	const upstream = new BroadcastProducer();
	req.serve(upstream.consume());
	expect(await pending).toBeDefined();

	// An exact-path local broadcast competes with them on cost too.
	const localRoute = origin.route(prefix, { identity: { id: IDENTITY, epoch: 0n }, cost: 5n });
	const local = localRoute.broadcast;
	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(
		WireRoute.normalize({ identity: IDENTITY, cost: 1n }),
	);
	localRoute.setCost(0n);
	expect(wireOf(consumer).advertised.peek()?.get(prefix)?.[0]?.route).toEqual(
		WireRoute.normalize({ identity: IDENTITY, cost: 0n }),
	);

	local.close();
	upstream.close();
	pricey.close();
	cheap.close();
	origin.close();
});

test("a shared reject still surfaces from demand", async () => {
	const origin = new Producer();
	const handle = origin.claim(Path.from("live"));
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
	const handle = origin.claim(Path.from("live"));
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
	second.serve(produced);
	await expect(pending).resolves.toBeDefined();

	handle.close();
	produced.close();
	origin.close();
});

test("advancing requested without settling rejects the previous request", async () => {
	const origin = new Producer();
	const handle = origin.claim(Path.from("live"));
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
	second.value.serve(produced);
	await expect(secondDemand).resolves.toBeDefined();

	handle.close();
	produced.close();
	origin.close();
});

test("a rejected request is not asked of the same route again", async () => {
	const origin = new Producer();
	const handle = origin.claim(Path.from("live"));
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
// one identity the request must hand over to it, not drop to nothing while the new session answers.
test("an outranked route keeps serving until its replacement answers", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("migrating");

	const older = new BroadcastProducer();
	const keepOlder = older.consume();
	const disposeOlder = serve(origin, path, () => keepOlder.clone(), WireRoute.normalize({ identity: IDENTITY }));

	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	let gaps = 0;
	const stop = request.active.subscribe((active) => {
		if (active === undefined) gaps++;
	});

	// The newer route wins the tie but has not answered yet.
	const newer = wireOf(origin).receive(path, { identity: IDENTITY });
	const asked = newer.requested().next();
	await settle();
	expect(request.active.peek()).toBe(first);

	// Once it answers, the request's tracks read it, under the same broadcast.
	const replacement = new BroadcastProducer();
	const { value: req } = await asked;
	req.serve(replacement.consume());
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

// Without an identity nothing says the newer route serves the same bytes: the request already open
// stays on its source, while a new one resolves the newer route on a fresh front.
test("a request stays on its source when another identity wins", async () => {
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

// A request left on a replaced identity still hears that identity go: it ends with an error
// rather than moving onto the winner, which may not hold the same bytes.
test("a request on a replaced identity ends once its source goes", async () => {
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

// Without an identity nothing says a standby serves the winner's bytes: the winner going ends the
// request instead of handing it to the standby.
// A request left on a replaced identity still fails over within that identity: another route
// of its identity serves the same bytes, so the request moves there rather than ending, while
// fresh requests resolve the newer identity.
test("a request on a replaced identity fails over to another route of it", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("replicated");
	const newer = Identity.parse("01900000-0000-7000-8000-000000000002");

	const primary = new BroadcastProducer();
	const keepPrimary = primary.consume();
	const disposePrimary = serve(origin, path, () => keepPrimary.clone(), WireRoute.normalize({ identity: IDENTITY }));
	const replica = new BroadcastProducer();
	const disposeReplica = serve(
		origin,
		path,
		provider(replica),
		WireRoute.normalize({ identity: IDENTITY, cost: 5n }),
	);
	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	const restarted = new BroadcastProducer();
	const disposeRestarted = serve(origin, path, provider(restarted), WireRoute.normalize({ identity: newer }));
	await settle();
	expect(request.active.peek()).toBe(first);

	disposePrimary();
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	expect(request.active.peek()).toBe(first);
	expect(first?.identity).toBe(IDENTITY);
	expect(readsFrom(first, replica)).toBe(true);

	const fresh = consumer.request(path);
	await settle();
	expect(fresh.active.peek()?.identity).toBe(newer);

	// The replica going too ends it: nothing else serves its identity.
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

// A local broadcast announced under the held identity is a replica too.
test("a request on a replaced identity fails over to a local replica of it", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("replicated");
	const newer = Identity.parse("01900000-0000-7000-8000-000000000002");

	const primary = new BroadcastProducer();
	const keepPrimary = primary.consume();
	const disposePrimary = serve(origin, path, () => keepPrimary.clone(), WireRoute.normalize({ identity: IDENTITY }));
	const localRoute = origin.route(path, { identity: { id: IDENTITY, epoch: 0n }, cost: 5n });
	const local = localRoute.broadcast;
	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	const restarted = new BroadcastProducer();
	const disposeRestarted = serve(origin, path, provider(restarted), WireRoute.normalize({ identity: newer }));
	await settle();
	expect(request.active.peek()).toBe(first);

	disposePrimary();
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	expect(request.active.peek()).toBe(first);
	expect(first?.identity).toBe(IDENTITY);
	expect(readsFrom(first, local)).toBe(true);

	const fresh = consumer.request(path);
	await settle();
	expect(fresh.active.peek()?.identity).toBe(newer);

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
	const newer = Identity.parse("01900000-0000-7000-8000-000000000002");

	const localRoute = origin.route(path, { identity: { id: IDENTITY, epoch: 0n }, cost: 2n });
	const local = localRoute.broadcast;
	const standby = wireOf(origin).receive(path, WireRoute.normalize({ identity: IDENTITY, cost: 5n }));
	const asked = standby.requested();
	const request = consumer.request(path);
	await settle();
	const first = request.active.peek();
	expect(first).toBeDefined();

	const restarted = new BroadcastProducer();
	// Another identity, cheaper, wins new requests.
	const disposeRestarted = serve(
		origin,
		path,
		provider(restarted),
		WireRoute.normalize({ identity: newer, cost: 1n }),
	);
	await settle();
	expect(request.active.peek()).toBe(first);

	// The local broadcast steps away, so the standby is asked to take over; it comes back first.
	localRoute.withdraw();
	const takeover = await asked.next();
	localRoute.announce();
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

// A GOAWAY ranks a session's routes last, whatever their cost: a preference state this node
// keeps, which readers (and the peers a relay forwards to) never see.
test("a draining route ranks below every other and still serves alone", async () => {
	const origin = new Producer();
	const path = Path.from("drained");

	const draining = new BroadcastProducer();
	const disposeDraining = serve(
		origin,
		path,
		provider(draining),
		WireRoute.normalize({ identity: IDENTITY, cost: 1n, draining: true }),
	);
	const routes = origin.consume().routes();
	const alone = await routes.next();
	expect(alone?.kind).toBe("start");
	expect(alone?.route.cost).toBe(1n);
	expect(alone?.route.draining).toBeUndefined();

	// A costlier route wins, since the draining one ranks last.
	const standby = new BroadcastProducer();
	const disposeStandby = serve(origin, path, provider(standby), WireRoute.normalize({ cost: 5n }));
	const replaced = await routes.next();
	expect(replaced?.kind).toBe("restart");
	expect(replaced?.route.cost).toBe(5n);

	// Without it, the draining route serves again as the last resort.
	disposeStandby();
	const restored = await routes.next();
	expect(restored?.kind).toBe("restart");
	expect(restored?.route.identity).toBe(IDENTITY);

	routes.close();
	disposeDraining();
	draining.close();
	standby.close();
	origin.close();
});

test("a request ends when its winning source goes, even with a standby", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("standby");

	const standby = new BroadcastProducer();
	const disposeStandby = serve(origin, path, provider(standby), WireRoute.normalize({ cost: 5n }));
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
	const narrow = origin.claim(Path.from("live"));
	const wide = origin.claim(Path.from(""));
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
	// One identity, so the narrower route serves the same identity and the request follows it.
	const disposeWide = serve(origin, Path.from(""), provider(served), WireRoute.normalize({ identity: IDENTITY }));

	const request = consumer.request(Path.from("live/cam"));
	await settle();
	const before = request.active.peek();
	expect(before).toBeDefined();

	// The narrower route is asked while the broad one keeps serving, then says no. A
	// costlier sibling at the same prefix could serve, but is never asked.
	const narrow = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n }, cost: 1n });
	const sibling = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n }, cost: 2n });
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
	const wide = origin.claim(Path.from(""));
	const wideRequests = wide.requested();

	const request = consumer.request(Path.from("live/cam"));
	const { value: stale } = await wideRequests.next();

	// A narrower route takes over the pending request before the broad one answers.
	const narrow = origin.claim(Path.from("live"));
	const { value: req } = await narrow.requested().next();
	stale?.reject(new Error("unserved"));
	await settle();
	expect(request.closed.peek()).toBeUndefined();

	const produced = new BroadcastProducer();
	req.serve(produced);
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
	const wide = origin.claim(Path.from("live"));
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
	const handle = origin.claim(Path.from("live"));
	const waiting = handle.requested().next();
	const request = origin.request(Path.from("live/cam"));
	const { value: req } = await waiting;
	expect(req).toBeDefined();

	handle.close();
	await settle();

	expect((await handle.requested().next()).done).toBe(true);
	expect(request.active.peek()).toBeUndefined();
	req.serve(new BroadcastProducer());
	await settle();
	expect(request.active.peek()).toBeUndefined();

	request.close();
	origin.close();
});

test("announced filters by arbitrary patterns and reports captures", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const broadcast = publish(origin, Path.from("room/alice"));
	const announced = consumer.routes(Path.Pattern.parse("room/*"));

	const update = await announced.next();
	expect(update?.prefix).toBe(Path.from("room/alice"));
	expect(update?.captures?.map((capture) => capture.text)).toEqual(["alice"]);

	announced.close();
	broadcast.close();
	origin.close();
});

test("announced lists full names while covering views and session advertisements keep overlaps", async () => {
	const origin = new Producer();
	const filter = Path.Pattern.parse("**/*.chat");
	const reader = origin.scope(Path.from("project"), Path.Prefixes.all()).consume();
	const cam = origin.claim(Path.from("project/cam.hang"));
	const overlap = origin.claim(Path.from("project/room/mic.hang"));
	const chat = origin.claim(Path.from("project/lobby.chat"));
	const selected = await announcedView(reader, filter);
	const covering = await announcedView(reader, filter, { covering: true });
	const all = [Path.from("cam.hang"), Path.from("lobby.chat"), Path.from("room/mic.hang")];
	expect([...selected.peek().keys()]).toEqual([Path.from("lobby.chat")]);
	expect([...covering.peek().keys()].sort()).toEqual(all);
	expect(Array.from(wireOf(reader).advertised.peek()?.keys() ?? []).sort()).toEqual(all);

	const nested = origin.claim(Path.from("project/room/lobby.chat"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(selected.peek().has(Path.from("room/lobby.chat"))).toBe(true);
	overlap.setCost(2n);
	overlap.close();
	nested.close();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...selected.peek().keys()]).toEqual([Path.from("lobby.chat")]);
	await selected.close();
	await covering.close();
	cam.close();
	chat.close();
	origin.close();
});

test("announced filters before rooting and retains lifecycle, routing and watch cleanup", async () => {
	const origin = new Producer();
	const root = Path.from("room/alice");
	const scoped = origin.scope(root, Path.Prefixes.all());
	const reader = scoped.consume();
	const interests: string[] = [];
	const unlisten = wireOf(origin).interests((prefix, wanted) => interests.push(`${wanted ? "+" : "-"}${prefix}`));
	const source = new BroadcastProducer();
	const broad = origin.claim(Path.from("room"));
	const stream = reader.routes();
	const events = stream[Symbol.asyncIterator]();
	const covering = await announcedView(reader, undefined, { covering: true });
	expect([...covering.peek().keys()]).toEqual([Path.empty()]);
	expect(Array.from(wireOf(reader).advertised.peek()?.keys() ?? [])).toEqual([Path.empty()]);

	const request = reader.request(Path.from("cam"));
	const incoming = await broad.requested().next();
	if (!incoming.value) throw new Error("missing served request");
	incoming.value.serve(source.consume());
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(request.active.peek()).toBeDefined();
	request.close();

	const exact = origin.claim(root, { identity: { id: IDENTITY, epoch: 0n } });
	expect(await events.next()).toMatchObject({
		value: { kind: "start", prefix: Path.empty(), captures: expect.any(Array) },
	});
	wireOf(exact).update({ identity: IDENTITY, cost: 2n });
	expect(await events.next()).toMatchObject({ value: { kind: "update", route: { cost: 2n } } });
	wireOf(exact).update({ identity: Identity.random() });
	expect(await events.next()).toMatchObject({ value: { kind: "restart" } });
	exact.close();
	expect(await events.next()).toMatchObject({ value: { kind: "end" } });
	stream.close();
	await covering.close();
	expect(await events.next()).toMatchObject({ done: true });
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(interests).toContain(`+${root}`);
	expect(interests).toContain(`-${root}`);
	unlisten();
	broad.close();
	source.close();
	origin.close();
	expect(wireOf(reader).advertised.peek()).toBeUndefined();
});

// A claim exactly at the path outranks one that only covers it, whatever else ranks them, as the
// Rust router's `above` does: the covering one is cheaper and ahead on priority here.
test("a claim nearer its path outranks a broader one", () => {
	const path = Path.from("src/derived");
	const exact = { ...WireRoute.default, identity: Identity.parse("a"), claimed: true, cost: 9n, priority: -5 };
	const broad = { ...WireRoute.default, identity: Identity.parse("b"), claimed: true, cost: 1n, above: 1n };
	const pick = (...routes: WireRoute[]) =>
		mostSpecific(
			path,
			(prefix) => (prefix === path ? routes : []),
			(route) => ({ ref: route, route }),
		);
	expect(pick(exact, broad)).toBe(exact);
	expect(pick(broad, exact)).toBe(exact);
	const level = { ...broad, above: 0n };
	expect(pick(exact, level)).toBe(level);
});

// A claim shown above its own prefix is filed deeper than it is: it counts for where it really is,
// so a broad claim shown deep never beats the more specific claim filed shallower.
test("the most specific claim wins whichever prefix shows it", () => {
	const path = Path.from("src/a/b/c");
	const broad = { ...WireRoute.default, claimed: true, above: 2n };
	const exact = { ...WireRoute.default, claimed: true };
	const at = new Map([
		[Path.from("src/a/b"), broad],
		[Path.from("src/a"), exact],
	]);
	const pick = (prefix: Path.Valid) => [at.get(prefix)].filter((route) => route !== undefined);
	expect(mostSpecific(path, pick, (route) => ({ ref: route, route }))).toBe(exact);
	// Claims as specific as each other tie to the most recently received, wherever each is filed.
	for (const newer of ["deep", "exact"]) {
		const deep = { ...WireRoute.default, claimed: true, above: 1n };
		const shallow = { ...WireRoute.default, claimed: true };
		for (const route of newer === "deep" ? [shallow, deep] : [deep, shallow]) arrival(route);
		const same = new Map([
			[Path.from("src/a/b"), deep],
			[Path.from("src/a"), shallow],
		]);
		const pickSame = (prefix: Path.Valid) => [same.get(prefix)].filter((route) => route !== undefined);
		expect(mostSpecific(path, pickSame, (route) => ({ ref: route, route }))).toBe(
			newer === "deep" ? deep : shallow,
		);
	}
});

// Claims of one prefix rank against each other wherever each is filed, as `best_route` in Rust: a
// draining claim filed deeper loses to a live one made at its prefix, and an identity's higher epoch
// at the prefix supersedes its older one filed deeper.
test("a claim filed deeper ranks against the claim of its prefix", () => {
	const path = Path.from("src/a/b");
	const identity = Identity.parse("i");
	const made = { ...WireRoute.default, identity, claimed: true, epoch: 2n, cost: 5n };
	for (const deep of [
		{ ...WireRoute.default, identity: Identity.parse("j"), claimed: true, above: 1n, draining: true, cost: 1n },
		{ ...WireRoute.default, identity, claimed: true, epoch: 1n, above: 1n, cost: 1n },
	]) {
		const at = new Map<Path.Valid, WireRoute>([
			[Path.from("src/a"), deep],
			[Path.from("src"), made],
		]);
		const pick = (prefix: Path.Valid) => [at.get(prefix)].filter((route) => route !== undefined);
		expect(mostSpecific(path, pick, (route) => ({ ref: route, route }))).toBe(made);
	}
});

// An announce stream rooted beneath claims of one prefix filed at several depths walks the whole
// tier, as a request does: an identity's older epoch filed deeper never wins back the root once
// its newer epoch is superseded by rank, as `TableCursor::best` in Rust.
test("a rooted announce stream ranks the whole tier of claims covering its root", async () => {
	const origin = new Producer();
	const a = Identity.parse("a");
	const b = Identity.parse("b");
	const newer = wireOf(origin).receive(Path.from("src"), { identity: a, epoch: 2n, hops: [PEER], cost: 10n });
	const other = wireOf(origin).receive(Path.from("src/a/b"), { identity: b, hops: [PEER], cost: 5n, above: 2n });
	const older = wireOf(origin).receive(Path.from("src/a"), {
		identity: a,
		epoch: 1n,
		hops: [PEER],
		cost: 1n,
		above: 1n,
	});
	const reader = origin.scope(Path.from("src/a/b"), Path.Prefixes.all()).consume();
	const covering = await announcedView(reader, undefined, { covering: true });
	expect(covering.peek().get(Path.empty())?.identity).toBe(b);
	await covering.close();
	for (const handle of [newer, other, older]) handle.close();
	origin.close();
});

// Claims of one tier filed at different depths that tie through every rank hash on the path a
// covering stream presents them at, as a request for it does, so the stream advertises what the
// request resolves to, as `a_covering_root_advertises_what_a_request_for_it_resolves` in Rust.
test("a covering root advertises what a request for it resolves", async () => {
	const root = Path.from("src/a");
	// Each pair in both orders, so a tie the hash settles goes to either filing.
	for (const [i, swap] of [...Array(16).keys()].flatMap((i) => [[i, false] as const, [i, true] as const])) {
		const origin = new Producer();
		const names = swap ? [`b${i}`, `a${i}`] : [`a${i}`, `b${i}`];
		const made = { ...WireRoute.default, identity: Identity.parse(names[0]), hops: [PEER], claimed: true };
		const filed = { ...made, identity: Identity.parse(names[1]), above: 1n };
		const handles = [wireOf(origin).receive(Path.from("src"), made), wireOf(origin).receive(root, filed)];
		const at = new Map<Path.Valid, WireRoute>([
			[Path.from("src"), made],
			[root, filed],
		]);
		const pick = (prefix: Path.Valid) => [at.get(prefix)].filter((route) => route !== undefined);
		// The claim filed at the path itself wins the tie, as a route entered here at the path skips
		// Rust's hash, whichever identity it carries.
		expect(mostSpecific(root, pick, (route) => ({ ref: route, route }))?.identity).toBe(filed.identity);
		const reader = origin.scope(root, Path.Prefixes.all()).consume();
		const covering = await announcedView(reader, undefined, { covering: true });
		expect(covering.peek().get(Path.empty())?.identity).toBe(filed.identity);
		await covering.close();
		for (const handle of handles) handle.close();
		origin.close();
	}
});

// Two claims of different identities at exactly the requested path skip the hash, as routes entered
// here at the path do in Rust and as the draft's Resolution says, so the newest wins whichever
// identity hashes lower: a publisher restarting under a new identity takes over at once.
test("the newest claim at the path wins a tie whatever its hash", () => {
	const path = Path.from("src/a");
	for (const i of Array(16).keys()) {
		const [a, b] = [`a${i}`, `b${i}`].map((name) => ({
			...WireRoute.default,
			identity: Identity.parse(name),
			hops: [PEER],
			claimed: true,
		}));
		// The candidates at the path, newest first, each received anew and in that order.
		const best = (...routes: WireRoute[]) => {
			const fresh = routes.map((route) => ({ route }));
			for (const entry of fresh.toReversed()) arrival(entry);
			return mostSpecific(
				path,
				(at) => (at === path ? fresh : []),
				(entry) => ({ ref: entry, route: entry.route }),
			)?.route;
		};
		expect(best(a, b)).toBe(a);
		expect(best(b, a)).toBe(b);
	}
});

// One claim reached at two filings ties through every rank and hash, since both carry its identity,
// so the most recently received wins wherever each is filed, as Rust's newest entry and the draft's
// Resolution have it.
test("a tie across filings goes to the most recently received claim", async () => {
	const root = Path.from("src/a/b");
	const identity = Identity.parse("cam");
	const deep = { ...WireRoute.default, identity, hops: [PEER], claimed: true, above: 1n };
	const shallow = { ...WireRoute.default, identity, hops: [toHop(11n)], claimed: true };
	const arrivals: [Path.Valid, WireRoute][][] = [
		[
			[Path.from("src/a"), deep],
			[Path.from("src"), shallow],
		],
		[
			[Path.from("src"), shallow],
			[Path.from("src/a"), deep],
		],
	];
	for (const order of arrivals) {
		const origin = new Producer();
		const handles = order.map(([prefix, route]) => wireOf(origin).receive(prefix, route));
		const reader = origin.scope(root, Path.Prefixes.all()).consume();
		const covering = await announcedView(reader, undefined, { covering: true });
		expect(covering.peek().get(Path.empty())?.hops).toEqual(order[1][1].hops);
		await covering.close();
		for (const handle of handles) handle.close();
		origin.close();
	}
});

// A local broadcast and a claim at the root tie on rank, and a request takes the local broadcast,
// so a covering stream rooted there shows it too, though the claim came later.
test("a covering root shows the local broadcast a request takes over an equal claim", async () => {
	const origin = new Producer();
	const path = Path.from("live");
	const local = origin.route(path, { identity: { id: IDENTITY, epoch: 0n }, cost: 5n });
	const reader = origin.scope(path, Path.Prefixes.all()).consume();
	const covering = await announcedView(reader, undefined, { covering: true });
	const claim = origin.claim(path, { cost: 5n });
	const request = origin.consume().request(path);
	await settle();
	expect(request.active.peek()?.identity).toBe(IDENTITY);
	expect(covering.peek().get(Path.empty())?.identity).toBe(IDENTITY);
	await covering.close();
	request.close();
	claim.close();
	local.close();
	origin.close();
});

// A claim announced after starting hidden, or announced again after a withdrawal, is received then,
// as Rust files a new entry for each announce, so beneath the path it yields a full tie across
// filings to the claim held longer.
test("a claim announced again ranks as newly received", async () => {
	const origin = new Producer();
	const root = Path.from("src/a/b");
	const identity = { id: Identity.parse("cam"), epoch: 0n };
	// Filed at `src` with Above 0, and through a scope at `src/a` with Above 1: one claim prefix.
	const shallow = origin.claim(Path.from("src"), { hidden: true, identity });
	const deep = origin.scope(Path.empty(), Path.Prefixes.parse(["src/a"])).claim(Path.from("src"), { identity });
	const reader = origin.scope(root, Path.Prefixes.all()).consume();
	const covering = await announcedView(reader, undefined, { covering: true });
	const shown = async () => {
		await settle();
		return covering.peek().get(Path.empty())?.above ?? 0n;
	};
	expect(await shown()).toBe(1n);
	shallow.announce();
	expect(await shown()).toBe(1n);
	deep.withdraw();
	deep.announce();
	expect(await shown()).toBe(0n);
	await covering.close();
	shallow.close();
	deep.close();
	origin.close();
});

// A draining route still reaches its origin, so it outranks one held unavailable, however cheap.
test("a draining route outranks an unavailable one", () => {
	const draining = { ...WireRoute.default, draining: true, cost: 5n };
	const held = { ...WireRoute.default, available: false, cost: 1n };
	expect(compareRoutes(draining, held)).toBeLessThan(0);
	expect(compareRoutes(held, draining)).toBeGreaterThan(0);
});

// An identity's epoch supersedes only within one claim, as Rust's `Epochs` keys by the claim's prefix:
// a broader claim shown at the path, at a higher epoch, is another claim than the exact one there.
test("a broader claim at a higher epoch does not supersede the exact one", () => {
	const path = Path.from("src/a");
	const identity = Identity.parse("i");
	const exact = { ...WireRoute.default, identity, claimed: true, epoch: 0n };
	const broad = { ...WireRoute.default, identity, claimed: true, epoch: 1n, above: 1n };
	// The best of `routes`, newest first, all filed at the path.
	const best = (...routes: WireRoute[]) =>
		mostSpecific(
			path,
			(at) => (at === path ? routes : []),
			(route) => ({ ref: route, route }),
		);
	expect(best(exact, broad)).toBe(exact);
	expect(best(broad, exact)).toBe(exact);
	// The same claim at a higher epoch still supersedes its older one.
	const newer = { ...exact, epoch: 2n };
	expect(best(newer, exact)).toBe(newer);
	expect(superseded(exact, [exact, newer])).toBe(true);
	expect(superseded(exact, [exact, broad])).toBe(false);
});

// A lite-07 claim ranks as the one hop to the peer that sent it in what the origin announces, as in
// what a request resolves: tied with a newer one-hop route, the newer wins both.
test("an announcement counts a lite-07 claim as one hop", async () => {
	const origin = new Producer();
	const room = Path.from("room");
	const b = Identity.parse("b");
	const older = wireOf(origin).receive(room, { ...WireRoute.default, identity: Identity.parse("a"), claimed: true });
	const newer = wireOf(origin).receive(room, { ...WireRoute.default, identity: b, hops: [PEER] });
	const announced = origin.consume().routes();
	expect(await announced.next()).toMatchObject({ prefix: room, route: { identity: b } });
	announced.close();
	older.close();
	newer.close();
	origin.close();
});

test("the spread hash matches rs/moq-net byte for byte", () => {
	expect(spreadHash("pool/job-0", "a")).toBe(0x3df8f75a44f8eefbn);
	expect(spreadHash("pool/job-0", "b")).toBe(0x3df8f85a44f8f0aen);
});

test("an equal-cost pool spreads its paths the same way on every node", async () => {
	const workers = [10n, 11n, 12n, 13n].map((id) => toHop(id));
	const paths = Array.from({ length: 64 }, (_, i) => Path.from(`pool/job-${i}`));

	// The worker each path resolves to on an origin whose pool arrived in `order`.
	async function winners(order: typeof workers): Promise<bigint[]> {
		const origin = new Producer();
		const served = new Map<Path.Valid, bigint>();
		const handles = order.map((hop) => {
			// Each worker is its own stream, named the same on every node.
			const identity = Identity.parse(`worker-${hop}`);
			const handle = wireOf(origin).receive(Path.from("pool"), { identity, hops: [hop], cost: 3n });
			void (async () => {
				for await (const request of handle.requested()) {
					served.set(request.path, hop);
					request.serve(new BroadcastProducer());
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
	].map((hops) => hops.map((id) => toHop(id)))) {
		const origin = new Producer();
		const served: bigint[] = [];
		const handles = order.map((hop) => {
			const handle = wireOf(origin).receive(path, { hops: [hop], cost: 3n });
			void (async () => {
				for await (const request of handle.requested()) {
					served.push(hop);
					request.serve(new BroadcastProducer());
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

// Beneath a prefix, a later route that ties the one in use never displaces it, whichever hop it
// names: of one named identity, or of identities sessions minted (moq-lite-06 carries none). A
// route that restarts under a fresh identity is new, so it yields the tie to one that kept its
// identity. As `an_equal_route_keeps_the_one_in_use` in rs/moq-net.
test("an equal route beneath its prefix keeps the one in use", async () => {
	const named = Identity.parse("worker");
	const at = (prefix: string) => Array.from({ length: 16 }, (_, i) => Path.from(`pool/${prefix}-${i}`));
	for (const minted of [false, true]) {
		for (const order of [
			[11n, 10n],
			[10n, 11n],
		].map((hops) => hops.map((id) => toHop(id)))) {
			const origin = new Producer();
			const served = new Map<Path.Valid, bigint>();
			const handles = order.map((hop) => {
				const identity = minted ? Mint.fresh() : named;
				const handle = wireOf(origin).receive(Path.from("pool"), { identity, hops: [hop], cost: 3n });
				void (async () => {
					for await (const request of handle.requested()) {
						served.set(request.path, hop);
						request.serve(new BroadcastProducer());
					}
				})();
				return handle;
			});
			const paths = at("job");
			const requests = paths.map((path) => origin.request(path));
			await settle();
			expect(paths.map((path) => served.get(path))).toEqual(paths.map(() => order[0]));

			if (minted) {
				// A lite-06 RESTART: its session mints a fresh identity for the route in use.
				const held = wireOf(defined(handles[0]));
				held.update({ ...held.info, identity: Mint.fresh(), epoch: 0n });
				const next = at("next");
				requests.push(...next.map((path) => origin.request(path)));
				await settle();
				expect(next.map((path) => served.get(path))).toEqual(next.map(() => order[1]));
			}

			for (const request of requests) request.close();
			for (const handle of handles) handle.close();
			origin.close();
		}
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

test("a rooted producer shares the table and enforces its prefixes", async () => {
	const origin = new Producer();
	const scoped = origin.scope(Path.from("tenant"), Path.Prefixes.parse(["room/alice", "room/carol"]));
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
	expect(() => scoped.route(Path.from("other"), { hidden: true }).broadcast).toThrow("outside the origin scope");
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => scoped.request(Path.from("room/bob"))).toThrow("outside the origin scope");
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => scoped.claim(Path.from("other"))).toThrow("outside the origin scope");
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => scoped.scope(Path.empty(), Path.Prefixes.parse(["other"]))).toThrow("do not overlap");
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => origin.scope(Path.empty(), new Path.Prefixes())).toThrow("do not overlap");

	const nested = scoped.scope(Path.from("room"), Path.Prefixes.all());
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(await announcedView(nested)).peek().keys()]).toEqual([Path.from("alice")]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => nested.route(Path.from("bob"), { hidden: true }).broadcast).toThrow("outside the origin scope");
	const announced = nested.routes();
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
	const scoped = origin.scope(Path.empty(), new Path.Prefixes([Path.Prefix.parse(Path.from("room"))]));
	const wide = origin.scope(Path.empty(), Path.Prefixes.all());
	const inside = publish(scoped, Path.from("room/alice"));
	const claim = scoped.claim(Path.from("room/claim"));
	const ranked = wide.claim(Path.from("ranked"));
	const peer = serve(scoped, Path.from("room/peer"), provider(new BroadcastProducer()));

	scoped.close();
	wide.close();

	expect(origin.closed.peek()).toBeUndefined();
	expect(owned.closed.peek()).toBeUndefined();
	expect(inside.closed.peek()).toBeNull();
	const routes = (await announcedView(origin)).peek();
	expect([...routes.keys()]).toEqual([Path.from("lobby")]);

	// A released handle refuses new work, so a borrower notices instead of publishing into nothing.
	expect(() => scoped.route(Path.from("room/late"), { hidden: true }).broadcast).toThrow("origin handle is closed");
	scoped.claim(Path.from("room/late")).close();
	expect((await announcedView(origin)).peek().has(Path.from("room/late"))).toBe(false);

	// The owner and fresh handles keep working.
	const again = publish(origin.scope(Path.empty(), Path.Prefixes.all()), Path.from("room/bob"));
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
	const scoped = origin.scope(Path.empty(), Path.Prefixes.all());
	const path = Path.from("room");
	const publication = scoped.route(path);
	const broadcast = publication.broadcast;
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
	const scoped = origin.scope(Path.empty(), Path.Prefixes.all());
	const path = Path.from("blind");

	const request = consumer.request(path);
	const detach = wireOf(scoped).attach(false);
	const snapshot = wireOf(scoped).snapshot(Path.empty());
	const interests: string[] = [];
	const unlisten = wireOf(scoped).interests((prefix, wanted) => interests.push(`${wanted ? "+" : "-"}${prefix}`));
	const announced = consumer.routes();
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
	consumer.follow(Path.from("later")).close();
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
	const scoped = origin.scope(Path.from("tenant"), Path.Prefixes.parse(["room/alice/chat"]));
	const dynamic = scoped.claim(Path.from("room"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	const denied = origin.request(Path.from("tenant/room/alice/video"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(denied.active.peek()).toBeUndefined();
	const request = origin.request(Path.from("tenant/room/alice/chat"));
	const requests = dynamic.requested();
	const pending = await requests.next();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(pending.value?.path).toBe(Path.from("room/alice/chat"));
	const broadcast = new BroadcastProducer();
	pending.value.serve(broadcast);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(request.active.peek()).toBeDefined();
	dynamic.setCost(2n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	// Filed at its scope, beneath the prefix it claims.
	expect((await announcedView(origin)).peek().get(Path.from("tenant/room/alice/chat"))?.cost).toBe(2n);
	await requests.return?.();
	dynamic.close();
	request.close();
	denied.close();
	broadcast.close();
	origin.close();
});

test("scoped wire views filter and rebase advertisements and blind requests", () => {
	const origin = new Producer();
	const scoped = origin.scope(Path.from("tenant"), Path.Prefixes.parse(["room"]));
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

// A claim broader than its scope is filed at each scope prefix beneath it, as many segments above
// it as the claim is broader, so it is advertised only where it may serve. Mirrors the Rust
// `a_claim_is_filed_at_the_grant_prefixes_beneath_it`.
test("a claim is filed at the scope prefixes beneath it", async () => {
	const origin = new Producer();
	const claim = origin.scope(Path.empty(), Path.Prefixes.parse(["a/b", "a/c/d", "z"])).claim(Path.from("a"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	const advertised = wireOf(origin.consume()).advertised.peek() ?? new Map();
	const filed = [...advertised].map(([path, adverts]) => [path, adverts[0]?.route.above ?? 0n]);
	expect(filed.sort()).toEqual([
		[Path.from("a/b"), 1n],
		[Path.from("a/c/d"), 2n],
	]);
	const denied = origin.request(Path.from("a/q"));
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(denied.active.peek()).toBeUndefined();
	denied.close();
	claim.close();
	origin.close();
});

test("a scoped dynamic is announced only to readers its scope can serve", async () => {
	const origin = new Producer();
	const dynamic = origin.scope(Path.empty(), Path.Prefixes.parse(["room/chat"])).claim(Path.empty());
	const chat = origin.scope(Path.empty(), Path.Prefixes.parse(["room/chat"]));
	const video = origin.scope(Path.empty(), Path.Prefixes.parse(["room/video"]));
	await new Promise<void>((resolve) => setImmediate(resolve));
	// Filed at its scope, beneath the prefix it claims.
	const filed = Path.from("room/chat");
	expect([...(await announcedView(chat, undefined, { covering: true })).peek().keys()]).toEqual([filed]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(await announcedView(video, undefined, { covering: true })).peek().keys()]).toEqual([]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(wireOf(chat.consume()).advertised.peek()?.keys() ?? [])]).toEqual([filed]);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect([...(wireOf(video.consume()).advertised.peek()?.keys() ?? [])]).toEqual([]);
	dynamic.close();
	origin.close();
});

test("a rooted reader presents the most specific covering route", async () => {
	const origin = new Producer();
	const narrow = origin.claim(Path.from("room/alice"), { cost: 9n });
	const broad = origin.claim(Path.from("room"), { cost: 1n });
	const rooted = origin.scope(Path.from("room/alice"), Path.Prefixes.all());
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announcedView(rooted)).peek().get(Path.empty())?.cost).toBe(9n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(wireOf(rooted.consume()).advertised.peek()?.get(Path.empty())?.[0]?.route.cost).toBe(9n);
	broad.close();
	narrow.close();
	origin.close();
});

// Filed deeper, a broader claim still gives way to a more specific one above the root.
test("a rooted reader presents the most specific claim, however deep each is filed", async () => {
	const origin = new Producer();
	const scoped = (prefix: string) => origin.scope(Path.empty(), Path.Prefixes.parse([prefix]));
	const narrow = scoped("room/alice").claim(Path.from("room"), { cost: 9n });
	const broad = scoped("room/alice/x").claim(Path.empty(), { cost: 1n });
	const rooted = origin.scope(Path.from("room/alice/x"), Path.Prefixes.all());
	await new Promise<void>((resolve) => setImmediate(resolve));
	const view = await announcedView(rooted, undefined, { covering: true });
	expect(view.peek().get(Path.empty())?.cost).toBe(9n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(wireOf(rooted.consume()).advertised.peek()?.get(Path.empty())?.[0]?.route.cost).toBe(9n);
	broad.close();
	narrow.close();
	origin.close();
});

test("a scoped reader picks the best route its scope can see at a prefix", async () => {
	const origin = new Producer();
	const scoped = (prefix: string) => origin.scope(Path.empty(), Path.Prefixes.parse([prefix]));
	const chat = scoped("room/chat").claim(Path.empty(), { cost: 1n });
	const video = scoped("room/video").claim(Path.empty(), { cost: 5n });
	const reader = scoped("room/video");

	await new Promise<void>((resolve) => setImmediate(resolve));

	// Each claim is filed at its own scope, so the reader sees the one filed within its own.
	const filed = Path.from("room/video");
	expect((await announcedView(reader, undefined, { covering: true })).peek().get(filed)?.cost).toBe(5n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(
		(await announcedView(origin, Path.Pattern.parse("room/video"), { covering: true })).peek().get(filed)?.cost,
	).toBe(5n);
	const announced = reader.routes(undefined, { covering: true });
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect((await announced.next())?.route.cost).toBe(5n);
	announced.close();

	// A session publishing the scoped view offers the video route too.
	const offered = [...(wireOf(reader.consume()).advertised.peek()?.get(filed) ?? [])];
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(offered.map((advert) => advert.route.cost)).toEqual([5n]);

	video.close();
	chat.close();
	origin.close();
});

test("broadcast handles carry the path they were created or requested at", async () => {
	expect(new BroadcastProducer().consume().path).toBe(Path.empty());

	const origin = new Producer();
	const scoped = origin.scope(Path.from("tenant"), Path.Prefixes.parse(["room"]));
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
	const dynamic = origin.claim(Path.from("live"));
	const request = origin.request(Path.from("live/bob"));
	const pending = await dynamic.requested().next();
	const served = new BroadcastProducer();
	pending.value.serve(served);
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

test("a higher epoch of a dynamic route supersedes a cheaper local broadcast of its identity", async () => {
	const origin = new Producer();
	const path = Path.from("camera");
	const identity = Identity.random();
	const localRoute = origin.route(path, { identity: { id: identity, epoch: 1n }, cost: 0n });
	const dynamic = origin.claim(path, { identity: { id: identity, epoch: 2n }, cost: 10n });
	const served = new BroadcastProducer();
	const requests = dynamic.requested();
	const request = origin.consume().request(path);
	const asked = await requests.next();
	expect(asked.value?.path).toBe(path);
	asked.value.serve(served);
	await settle();
	expect(readsFrom(request.active.peek(), served)).toBe(true);
	request.close();
	await requests.return?.();
	dynamic.close();
	localRoute.broadcast.close();
	served.close();
	origin.close();
});

test("a higher epoch that is not the best route still supersedes a local broadcast", async () => {
	const origin = new Producer();
	const path = Path.from("camera");
	const stale = Identity.random();
	const localRoute = origin.route(path, { identity: { id: stale, epoch: 1n }, cost: 0n });
	const newer = origin.claim(path, { identity: { id: stale, epoch: 2n }, cost: 10n });
	const other = origin.claim(path, { identity: { id: Identity.random(), epoch: 0n }, cost: 5n });
	const served = new BroadcastProducer();
	const requests = other.requested();
	const request = origin.consume().request(path);
	// The stale local broadcast is ineligible, so the cheapest eligible route answers.
	const asked = await requests.next();
	asked.value.serve(served);
	await settle();
	expect(readsFrom(request.active.peek(), served)).toBe(true);
	request.close();
	await requests.return?.();
	other.close();
	newer.close();
	localRoute.broadcast.close();
	served.close();
	origin.close();
});

test("a publisher weighs a local broadcast against the routes it may forward only", () => {
	const origin = new Producer();
	const path = Path.from("camera");
	const identity = Identity.random();
	const localRoute = origin.route(path, { identity: { id: identity, epoch: 1n }, cost: 0n });
	const originated = origin.claim(path, { cost: 5n });
	// A session delivered the higher epoch, which a publisher never forwards back out.
	const received = wireOf(origin).receive(path, { identity, epoch: 2n, cost: 10n, hops: [PEER] });
	expect(wireOf(origin.consume()).local(path, identity)).toBeDefined();
	received.close();
	originated.close();
	localRoute.broadcast.close();
	origin.close();
});

// A full tie beneath the path holds the claim received first, as a request does; the scope's
// merged root row lists that claim first, which `presented` must not read as the newest.
test("a scope's root row presents the tied claim a request holds", async () => {
	const origin = new Producer();
	const identity = { id: Identity.parse("cam"), epoch: 0n };
	const older = origin.claim(Path.from("src"), { identity });
	const newer = origin.claim(Path.from("src"), { identity });
	const reader = origin.scope(Path.from("src/a"), Path.Prefixes.all()).consume();
	await settle();
	const all = wireOf(origin.consume()).advertised.peek()?.get(Path.from("src"));
	const table = wireOf(reader).advertised.peek();
	if (!all || !table) throw new Error("expected a table");
	// Newest first at its own filing.
	const held = all[1];
	expect(presented(Path.empty(), table, () => true, { root: Path.from("src/a") }).get(Path.empty())?.ref).toBe(
		held.ref,
	);
	const request = origin.consume().request(Path.from("src/a"));
	expect(await askedFirst(Path.from("src/a"), [older, newer])).toBe(older);
	request.close();
	older.close();
	newer.close();
	origin.close();
});

// A broadcast published above a scope's root serves only its own path, so the merged root row,
// whose first eligible entry an unsolicited PUBLISH_NAMESPACE offers, leads with the costlier claim
// of the same prefix that serves the root.
test("a scope's root row leads with a claim that serves it over a broadcast above it", async () => {
	const origin = new Producer();
	const broadcast = origin.route(Path.from("src"));
	const filed = origin.scope(Path.empty(), Path.Prefixes.parse(["src/a"])).claim(Path.from("src"), { cost: 5n });
	const reader = origin.scope(Path.from("src/a"), Path.Prefixes.all()).consume();
	await settle();
	const row = wireOf(reader).advertised.peek()?.get(Path.empty());
	if (!row) throw new Error("expected a root row");
	expect(row.map((entry) => entry.route.identity)).toEqual([filed.identity, broadcast.identity]);
	broadcast.close();
	filed.close();
	origin.close();
});

/** Whether `claim` was asked to serve anything, checked after the table settles. */
function watchAsked(claim: { requested(): AsyncIterable<unknown> }): () => boolean {
	let asked = false;
	void (async () => {
		for await (const _ of claim.requested()) asked = true;
	})();
	return () => asked;
}

// A broadcast published above a scope's root is the most specific claim on it, so the root row
// leads with it, and a request for the root, which it does not serve, goes no further than it.
test("a broadcast above a request shadows a broader claim, as the root row announces", async () => {
	const origin = new Producer();
	const broadcast = origin.route(Path.from("src"));
	const broad = origin.claim(Path.empty());
	const asked = watchAsked(broad);
	const reader = origin.scope(Path.from("src/a"), Path.Prefixes.all()).consume();
	const covering = await announcedView(reader, undefined, { covering: true });
	await settle();
	expect(wireOf(reader).advertised.peek()?.get(Path.empty())?.[0]?.route.identity).toBe(broadcast.identity);
	expect(covering.peek().get(Path.empty())?.identity).toBe(broadcast.identity);
	await covering.close();
	const demanded = wireOf(reader).demand(Path.empty());
	await settle();
	expect(asked()).toBe(false);
	await expect(demanded).resolves.toBeUndefined();
	// The broadcast still serves its own path.
	expect(await wireOf(origin.consume()).demand(Path.from("src"))).toBeDefined();
	broadcast.close();
	broad.close();
	origin.close();
});

// A broadcast's announce and retract change what every path beneath it resolves to.
test("a request beneath a broadcast resolves the broader claim once the broadcast goes", async () => {
	const origin = new Producer();
	const broad = origin.claim(Path.empty());
	const asked = watchAsked(broad);
	const broadcast = origin.route(Path.from("src"));
	const request = origin.consume().request(Path.from("src/a"));
	await settle();
	expect(asked()).toBe(false);
	broadcast.close();
	await settle();
	expect(asked()).toBe(true);
	request.close();
	broad.close();
	origin.close();
});

// Rust's a_broadcast_here_shadows_a_broader_claim_shown_at_its_prefix, with a claim of the
// broadcast's own prefix filed deeper: of one tier, a claim that serves the request outranks the
// broadcast, unless the broadcast carries its identity at a higher epoch.
for (const superseding of [false, true]) {
	test(`a broadcast above a request ${superseding ? "supersedes an older epoch of its identity filed deeper" : "ranks behind a tier-mate that serves the request"}`, async () => {
		const origin = new Producer();
		const identity = Identity.random();
		const filed = origin
			.scope(Path.empty(), Path.Prefixes.parse(["src/a"]))
			.claim(Path.from("src"), { identity: { id: identity, epoch: 1n } });
		const broadcast = origin.route(Path.from("src"), superseding ? { identity: { id: identity, epoch: 2n } } : {});
		const asked = watchAsked(filed);
		const demanded = wireOf(origin.consume()).demand(Path.from("src/a/b"));
		await settle();
		expect(asked()).toBe(!superseding);
		if (superseding) await expect(demanded).resolves.toBeUndefined();
		broadcast.close();
		filed.close();
		origin.close();
		await demanded.catch(() => {});
	});
}

// Differential: a request beneath broadcasts and claims, some filed deeper than their prefix,
// resolves to the identity a covering announcement presents for its path, or is refused when that
// is a broadcast above it.
test("a request resolves to the identity its covering announcement presents", async () => {
	const root = Path.from("a/b/c");
	const prefixes = [Path.empty(), Path.from("a"), Path.from("a/b"), root];
	const identities = [Identity.random(), Identity.random()];
	type Advertised = { route: { identity: Identity.Valid; epoch: bigint; cost: bigint } };
	const route = fc.record({
		identity: fc.constantFrom(...identities),
		epoch: fc.bigInt({ min: 0n, max: 2n }),
		cost: fc.bigInt({ min: 0n, max: 3n }),
	});
	// A broadcast's depth (1 to 3), and a claim's depth with how much deeper it is filed.
	const broadcasts = fc.uniqueArray(fc.record({ depth: fc.integer({ min: 1, max: 3 }), route }), {
		selector: (item) => item.depth,
		maxLength: 3,
	});
	const claims = fc.array(
		fc
			.record({ depth: fc.integer({ min: 0, max: 3 }), deeper: fc.integer({ min: 0, max: 3 }), route })
			.map((item) => ({ ...item, deeper: Math.min(item.deeper, 3 - item.depth) })),
		{ maxLength: 4 },
	);
	const run = async (
		published: readonly (Advertised & { depth: number })[],
		claimed: readonly (Advertised & { depth: number; deeper: number })[],
	) => {
		const origin = new Producer();
		const options = (item: Advertised) => ({
			cost: item.route.cost,
			identity: { id: item.route.identity, epoch: item.route.epoch },
		});
		const routes = published.map((item) => ({
			...item,
			handle: origin.route(prefixes[item.depth], options(item)),
		}));
		const asked: Identity.Valid[] = [];
		const handles = claimed.map((item) => {
			const scope = origin.scope(Path.empty(), Path.Prefixes.parse([prefixes[item.depth + item.deeper]]));
			const handle = scope.claim(prefixes[item.depth], options(item));
			void (async () => {
				for await (const _ of handle.requested()) asked.push(item.route.identity);
			})();
			return handle;
		});
		const reader = origin.scope(root, Path.Prefixes.all()).consume();
		const covering = await announcedView(reader, undefined, { covering: true });
		const presented = covering.peek().get(Path.empty());
		const demanded = wireOf(reader).demand(Path.empty());
		await settle();
		const exact = routes.find((item) => item.depth === 3);
		const local = await Promise.race([demanded, settle().then(() => "pending" as const)]);
		const resolved =
			local === "pending"
				? asked[0]
				: local !== undefined && exact && readsFrom(local, exact.handle.broadcast)
					? exact.handle.identity
					: undefined;
		// Which advertisement is presented, by its identity and epoch: a broadcast above the root
		// serves nothing there. A broadcast above and a serving claim alike would be ambiguous.
		const named = (item: Advertised) =>
			presented !== undefined &&
			item.route.identity === presented.identity &&
			item.route.epoch === (presented.epoch ?? 0n);
		const shadowing = routes.some((item) => item.depth < 3 && named(item));
		await covering.close();
		for (const item of routes) item.handle.close();
		for (const handle of handles) handle.close();
		origin.close();
		await demanded.catch(() => {});
		fc.pre(!(shadowing && (claimed.some(named) || (exact !== undefined && named(exact)))));
		expect(resolved).toBe(shadowing ? undefined : presented?.identity);
	};
	await fc.assert(fc.asyncProperty(broadcasts, claims, run), { seed: 0x6d6f71, numRuns: 300 });
});

// A request pinned to an identity ranks only that identity's claims, so a broadcast of another
// identity above it shadows nothing.
test("a broadcast above a pinned request of another identity shadows nothing", async () => {
	const origin = new Producer();
	const broadcast = origin.route(Path.from("src"));
	const broad = origin.claim(Path.empty());
	const demanded = wireOf(origin.consume()).demand(Path.from("src/a"), broad.identity);
	await askedFirst(Path.from("src/a"), [broad]);
	broadcast.close();
	broad.close();
	origin.close();
	await demanded.catch(() => {});
});

test("a scope's merged covering prefixes never supersede across prefixes", async () => {
	const origin = new Producer();
	const identity = Identity.random();
	const room = origin.claim(Path.from("room"), { identity: { id: identity, epoch: 2n } });
	const video = origin.claim(Path.from("room/video"), { identity: { id: identity, epoch: 1n } });
	const reader = origin.scope(Path.from("room/video"), Path.Prefixes.all()).consume();
	await settle();
	const table = wireOf(reader).advertised.peek();
	if (!table) throw new Error("expected a table");
	// The more specific prefix wins, whatever epoch the covering one carries.
	expect(presented(Path.empty(), table, () => true).get(Path.empty())?.route.epoch).toBe(1n);
	video.close();
	room.close();
	origin.close();
});

test("supersession only counts routes the reader can see", async () => {
	const origin = new Producer();
	const identity = Identity.random();
	const video = origin
		.scope(Path.empty(), Path.Prefixes.parse(["room/video"]))
		.claim(Path.empty(), { identity: { id: identity, epoch: 1n } });
	const live = await announcedView(origin, Path.Pattern.parse("room/video"), { covering: true });
	await settle();
	expect(live.peek().size).toBe(1);
	// A higher epoch the reader cannot see never hides the route it can.
	const chat = origin
		.scope(Path.empty(), Path.Prefixes.parse(["room/chat"]))
		.claim(Path.empty(), { identity: { id: identity, epoch: 2n } });
	await settle();
	expect([...live.peek().values()].map((route) => route.epoch)).toEqual([1n]);
	chat.close();
	video.close();
	origin.close();
});

test("the newest epoch of an identity is announced in place over a cheaper one", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room/alice");
	const announced = consumer.routes();
	const identity = Identity.random();

	const oldRoute = origin.route(path, { identity: { id: identity, epoch: 1n }, cost: 1n });
	const old = oldRoute.broadcast;
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start" });

	// A higher epoch of the same identity supersedes it despite the cost: the same stream.
	const newer = wireOf(origin).receive(path, { identity, epoch: 2n, hops: [PEER], cost: 9n });
	expect(await announced.next()).toMatchObject({
		prefix: path,
		kind: "update",
		route: { identity, epoch: 2n, hops: [PEER] },
	});

	// The older one still stands, so it comes back once the newer one goes.
	newer.close();
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "update", route: { identity, epoch: 1n } });

	old.close();
	announced.close();
	origin.close();
});

const NEWER = Identity.parse("01900000-0000-7000-8000-000000000002");

test("demand held across an identity change asks again, unless it named the old identity", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("live/cam");
	const handle = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n } });
	const it = handle.requested();
	const pinned = wireOf(consumer).demand(path, IDENTITY);
	const unpinned = wireOf(consumer).demand(path);
	const { value: req } = await it.next();

	// Another identity now serves the route: the old handler's answer is for the old one.
	wireOf(handle).update({ ...wireOf(handle).info, identity: NEWER });
	req.serve(new BroadcastProducer().consume());
	await expect(pinned).rejects.toThrow("unroutable");

	const next = new BroadcastProducer();
	const track = defined(next.createTrack("video"));
	(await it.next()).value.serve(next);
	const subscription = (await unpinned)?.track("video").subscribe();
	hooks.createEmpty(track, 7n);
	expect((await subscription?.recvGroup())?.start).toBe(7n);

	subscription?.close();
	await it.return?.();
	next.close();
	handle.close();
	origin.close();
});

test("a demand refused across an identity change does not lose the carried demand's answer", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("live/cam");
	const handle = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n } });
	const it = handle.requested();
	const unpinned = wireOf(consumer).demand(path);
	const pinned = wireOf(consumer).demand(path, IDENTITY);
	await it.next();

	wireOf(handle).update({ ...wireOf(handle).info, identity: NEWER });
	await expect(pinned).rejects.toThrow("unroutable");
	// The replacement's refusal reaches the demand that carried over.
	(await it.next()).value?.reject(new Error("gone"));
	await expect(unpinned).rejects.toThrow("gone");

	await it.return?.();
	handle.close();
	origin.close();
});

test("demand held across an identity round trip still asks again", async () => {
	const origin = new Producer();
	const path = Path.from("live/cam");
	const handle = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n } });
	const it = handle.requested();
	const demanded = wireOf(origin.consume()).demand(path);
	const { value: req } = await it.next();

	// Released by the first change; the identity coming back does not undo that.
	wireOf(handle).update({ ...wireOf(handle).info, identity: NEWER });
	wireOf(handle).update({ ...wireOf(handle).info, identity: IDENTITY });
	req.serve(new BroadcastProducer().consume());

	const next = new BroadcastProducer();
	const track = defined(next.createTrack("video"));
	(await it.next()).value.serve(next);
	const subscription = (await demanded)?.track("video").subscribe();
	hooks.createEmpty(track, 7n);
	expect((await subscription?.recvGroup())?.start).toBe(7n);

	subscription?.close();
	await it.return?.();
	next.close();
	handle.close();
	origin.close();
});

test("re-pricing from the current route keeps the identity", () => {
	const origin = new Producer();
	const handle = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n }, cost: 5n });
	handle.setCost(1n);
	expect(wireOf(handle).info).toEqual(WireRoute.normalize({ identity: IDENTITY, cost: 1n }));

	const broadcastRoute = origin.route(Path.from("room"), {
		hidden: true,
		identity: { id: IDENTITY, epoch: 0n },
		cost: 5n,
	});
	const broadcast = broadcastRoute.broadcast;
	expect(wireOf(broadcastRoute).info).toBeUndefined();
	broadcastRoute.announce();
	broadcastRoute.setCost(1n);
	expect(wireOf(broadcastRoute).info).toEqual(WireRoute.normalize({ identity: IDENTITY, cost: 1n }));
	broadcastRoute.withdraw();
	expect(wireOf(broadcastRoute).info).toBeUndefined();

	broadcast.close();
	handle.close();
	origin.close();
});

test("a peer naming another identity is refused rather than served a different identity", async () => {
	const origin = new Producer();
	const consumer = origin.consume();
	const path = Path.from("room");
	const localRoute = origin.route(path, { identity: { id: IDENTITY, epoch: 0n } });
	const local = localRoute.broadcast;

	expect(wireOf(consumer).local(path, IDENTITY)).toBeDefined();
	const other = Identity.parse("01900000-0000-7000-8000-000000000002");
	expect(wireOf(consumer).local(path, other)).toBeUndefined();
	await expect(wireOf(consumer).demand(path, other)).rejects.toThrow("unroutable");

	local.close();
	origin.close();
});

test("a newer identity ends pinned and unpinned requests on the replaced publisher alike", () => {
	const origin = new Producer();
	const path = Path.from("room");
	const first = origin.route(path, { identity: { id: IDENTITY, epoch: 0n } });
	const reader = origin.consume();
	const pinned = reader.request(path, { identity: IDENTITY });
	const unpinned = reader.request(path);
	expect(pinned.active.peek()?.identity).toBe(IDENTITY);
	expect(unpinned.active.peek()?.identity).toBe(IDENTITY);
	const replacement = Identity.parse("01900000-0000-7000-8000-000000000002");
	origin.route(path, { identity: { id: replacement, epoch: 0n } });
	expect(() => first.announce()).toThrow("broadcast is closed");
	first.withdraw();
	expect(pinned.active.peek()).toBeUndefined();
	// The replaced publisher closed: the unpinned request ends rather than move to the newer one.
	expect(unpinned.active.peek()).toBeUndefined();
	// A fresh unpinned request names the publisher it actually resolved.
	const fresh = reader.request(path);
	expect(fresh.active.peek()?.identity).toBe(replacement);
	fresh.close();
	const stale = reader.request(path, { identity: IDENTITY });
	expect(stale.active.peek()).toBeUndefined();
	stale.close();
	pinned.close();
	unpinned.close();
	origin.close();
});

test("a request held across an identity change asks again and never takes the old handler's answer", async () => {
	const origin = new Producer();
	const prefix = Path.from("room");
	const path = Path.from("room/cam");
	const dynamic = origin.claim(prefix, { identity: { id: IDENTITY, epoch: 0n } });
	const unpinned = origin.consume().request(path);
	const pinned = origin.consume().request(path, { identity: IDENTITY });
	const requests = dynamic.requested();
	const old = (await requests.next()).value;
	wireOf(dynamic).update({ ...wireOf(dynamic).info, identity: NEWER });
	const current = origin.consume().request(path, { identity: NEWER });
	old.serve(new BroadcastProducer().consume());
	await settle();
	expect(pinned.active.peek()).toBeUndefined();
	expect(unpinned.active.peek()).toBeUndefined();
	expect(current.active.peek()).toBeUndefined();
	const next = (await requests.next()).value;
	next.serve(new BroadcastProducer().consume());
	await settle();
	expect(unpinned.active.peek()?.identity).toBe(NEWER);
	expect(current.active.peek()?.identity).toBe(NEWER);
	expect(pinned.active.peek()).toBeUndefined();
	await requests.return?.();
	unpinned.close();
	pinned.close();
	current.close();
	dynamic.close();
	origin.close();
});

test.each([
	["named", IDENTITY],
	["minted", undefined],
])("an identity update from a %s one restarts the claim, while a re-price keeps every handle", async (_, named) => {
	const origin = new Producer();
	const prefix = Path.from("live");
	const path = Path.from("live/cam");
	const dynamic = origin.claim(prefix, { identity: named && { id: named, epoch: 0n } });
	// A claim naming no identity serves one of its own.
	const from = dynamic.identity;
	expect(from).toBeDefined();
	if (named !== undefined) expect(from).toBe(named);
	const to = NEWER;
	const requests = dynamic.requested();
	const announced = origin.consume().routes();
	expect(await announced.next()).toMatchObject({ prefix, kind: "start" });

	const held = origin.consume().request(path);
	const old = new BroadcastProducer();
	const oldTrack = defined(old.createTrack("video"));
	(await requests.next()).value.serve(old);
	await settle();
	const resolved = held.active.peek();
	expect(resolved?.identity).toBe(from);

	const sticky = resolved?.track("video").subscribe();
	hooks.appendNext(oldTrack).finish();
	expect((await sticky?.recvGroup())?.start).toBe(0n);

	// A re-price at the same identity keeps every handle, and the handler is not asked again.
	dynamic.setCost(5n);
	expect(await announced.next()).toMatchObject({ prefix, kind: "update" });
	expect(held.active.peek()).toBe(resolved);
	const same = origin.consume().request(path);
	await settle();
	expect(same.active.peek()?.identity).toBe(from);
	const pinned = origin.consume().request(path, { identity: from });

	// Another identity is another stream: resolved requests keep the old one.
	wireOf(dynamic).update({ ...wireOf(dynamic).info, identity: to });
	expect(await announced.next()).toMatchObject({ prefix, kind: "restart" });
	await settle();
	expect(held.active.peek()).toBe(resolved);
	expect(held.closed.peek()).toBeUndefined();
	expect(same.active.peek()?.identity).toBe(from);
	expect(pinned.active.peek()?.identity).toBe(from);

	// The subscription already open stays on the old answer.
	hooks.appendNext(oldTrack).finish();
	expect((await sticky?.recvGroup())?.start).toBe(1n);

	// A re-request, local or from a session, asks the handler again and never reads the
	// old answer.
	const fresh = origin.consume().request(path);
	const demanded = wireOf(origin.consume()).demand(path);
	const next = new BroadcastProducer();
	const nextTrack = defined(next.createTrack("video"));
	(await requests.next()).value.serve(next);
	await settle();
	expect(fresh.active.peek()?.identity).toBe(to);
	const subscription = fresh.active.peek()?.track("video").subscribe();
	const forwarded = (await demanded)?.track("video").subscribe();
	hooks.createEmpty(nextTrack, 7n);
	expect((await subscription?.recvGroup())?.start).toBe(7n);
	expect((await forwarded?.recvGroup())?.start).toBe(7n);

	// Existing request handles can still subscribe, but never join the new answer.
	const retained = held.active.peek()?.track("video").subscribe();
	expect((await retained?.recvGroup())?.start).toBe(1n);
	hooks.appendNext(oldTrack).finish();
	expect((await retained?.recvGroup())?.start).toBe(2n);
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

test("an identity update does not retain an answer that already closed", async () => {
	const origin = new Producer();
	const path = Path.from("live/cam");
	const dynamic = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n } });
	const requests = dynamic.requested();
	const held = origin.consume().request(path);
	const old = new BroadcastProducer();
	(await requests.next()).value.serve(old);
	old.close();
	// The server's closure callback has not removed the answer yet.
	wireOf(dynamic).update({ identity: NEWER });
	try {
		expect(held.closed.peek()).toBeInstanceOf(Error);
		expect(held.active.peek()).toBeUndefined();
		const fresh = origin.consume().request(path);
		const next = new BroadcastProducer();
		(await requests.next()).value.serve(next);
		expect(fresh.active.peek()?.identity).toBe(NEWER);
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
	const dynamic = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n } });
	const requests = dynamic.requested();
	const held = origin.consume().request(path);
	const old = new SessionBroadcast();
	(await requests.next()).value.serve(old);
	// Change identities before the active getter or its subscription can clone the answer.
	wireOf(dynamic).update({ identity: NEWER });
	expect(old.closed.peek()).toBeUndefined();
	expect(held.active.peek()?.identity).toBe(IDENTITY);
	held.close();
	await old.closed;
	expect(held.active.peek()).toBeUndefined();
	await requests.return?.();
	dynamic.close();
	origin.close();
});

test("a session's re-request after an identity update asks the handler again", async () => {
	const origin = new Producer();
	const path = Path.from("live/cam");
	const dynamic = origin.claim(Path.from("live"), { identity: { id: IDENTITY, epoch: 0n } });
	const requests = dynamic.requested();
	const wire = wireOf(origin.consume());

	const before = wire.demand(path);
	const old = new BroadcastProducer();
	(await requests.next()).value.serve(old);
	expect(await before).toBeDefined();

	wireOf(dynamic).update({ ...wireOf(dynamic).info, identity: NEWER });
	const after = wire.demand(path);
	const next = new BroadcastProducer();
	const track = defined(next.createTrack("video"));
	(await requests.next()).value.serve(next);
	const subscription = (await after)?.track("video").subscribe();
	hooks.createEmpty(track, 7n);
	expect((await subscription?.recvGroup())?.start).toBe(7n);

	subscription?.close();
	await requests.return?.();
	old.close();
	next.close();
	dynamic.close();
	origin.close();
});

test("an update does not override route selection: another claim at the identity keeps it", async () => {
	const origin = new Producer();
	const prefix = Path.from("live");
	const path = Path.from("live/cam");
	const first = origin.claim(prefix, { identity: { id: IDENTITY, epoch: 0n }, cost: 1n });
	const second = origin.claim(prefix, { identity: { id: IDENTITY, epoch: 0n }, cost: 5n });
	const firstRequests = first.requested();
	const secondRequests = second.requested();

	const held = origin.consume().request(path);
	const answer = new BroadcastProducer();
	(await firstRequests.next()).value.serve(answer);
	await settle();
	expect(held.active.peek()?.identity).toBe(IDENTITY);

	// The first claim moves to another identity priced above the second, which still serves
	// the old one: the resolved request keeps its answer, and new requests go through the
	// second claim.
	wireOf(first).update({ ...wireOf(first).info, identity: NEWER, cost: 9n });
	const fresh = origin.consume().request(path);
	const replacement = new BroadcastProducer();
	const track = defined(replacement.createTrack("video"));
	(await secondRequests.next()).value.serve(replacement);
	await settle();
	expect(held.active.peek()?.identity).toBe(IDENTITY);
	expect(fresh.active.peek()?.identity).toBe(IDENTITY);
	const subscription = fresh.active.peek()?.track("video").subscribe();
	hooks.createEmpty(track, 7n);
	expect((await subscription?.recvGroup())?.start).toBe(7n);

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

test("a route naming no identity resolves a handle under the one it minted", () => {
	const origin = new Producer();
	const path = Path.from("room");
	const broadcast = origin.route(path);
	const request = origin.consume().request(path);
	expect(broadcast.identity).toBeDefined();
	expect(request.active.peek()?.identity).toBe(broadcast.identity);
	request.close();
	origin.close();
});

test("public announcements saturate static costs and refuse invalid cost shapes", async () => {
	const origin = new Producer();
	const path = Path.from("priced");
	const broadcastRoute = origin.route(path, { hidden: true });
	const broadcast = broadcastRoute.broadcast;
	const view = await announcedView(origin);
	broadcastRoute.setCost(2n ** 64n - 1n);
	broadcastRoute.announce();
	await settle();
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(view.peek().get(path)?.cost).toBe(2n ** 62n - 1n);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => broadcastRoute.setCost(-1n)).toThrow(RangeError);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(() => broadcastRoute.setCost({ warm: 1n, cold: 2n } as unknown as Cost)).toThrow(RangeError);
	broadcast.close();
	origin.close();
});

// A plain JavaScript caller can skip `Identity.parse`, so the origin checks a named identity's
// text before a peer refuses it.
test("routes and claims refuse an unchecked identity a peer would refuse", () => {
	const origin = new Producer();
	for (const text of ["", "a".repeat(257)]) {
		const identity = { id: text as Identity.Named, epoch: 0n };
		expect(() => origin.route(Path.from("named"), { identity })).toThrow(RangeError);
		expect(() => origin.claim(Path.from("named"), { identity })).toThrow(RangeError);
	}
	origin.close();
});

test("published names refuse navigation segments", () => {
	const origin = new Producer();
	try {
		for (const name of ["room/./cam", "room/../cam", ".", ".."]) {
			const path = Path.from(name);
			expect(() => origin.route(path, { hidden: true }).broadcast).toThrow();
			expect(() => origin.claim(path)).toThrow();
			expect(() => wireOf(origin).receive(path, WireRoute.default)).toThrow();
		}
		const scoped = origin.scope(Path.from("room/.."), Path.Prefixes.all());
		expect(() => scoped.route(Path.from("cam"), { hidden: true }).broadcast).toThrow();
		for (const path of ["room/.pro/cam", "room/a..b", "room/cam"]) {
			origin.route(Path.from(path), { hidden: true }).broadcast.close();
		}
	} finally {
		origin.close();
	}
});

import { expect, test } from "bun:test";
import * as Announce from "./announced.ts";
import type { Producer as BroadcastProducer } from "./broadcast.ts";
import { Route } from "./hop.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { wireOf } from "./wire.ts";

function publish(origin: OriginProducer, path: Path.Valid): BroadcastProducer {
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	return broadcast;
}

const p = (s: string) => Path.from(s);

test("next streams every appended event in order", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	const route = Route.default;
	producer.append({ prefix: p("a"), captures: undefined, kind: "start", route });
	producer.append({ prefix: p("a"), captures: undefined, kind: "end", route });

	expect(await consumer.next()).toEqual({ prefix: p("a"), captures: undefined, kind: "start", route });
	expect(await consumer.next()).toEqual({ prefix: p("a"), captures: undefined, kind: "end", route });
});

test("the consumer is an async iterable of the same events", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	const route = Route.default;
	producer.append({ prefix: p("a"), captures: undefined, kind: "start", route });
	producer.append({ prefix: p("a"), captures: undefined, kind: "end", route });

	const events = consumer[Symbol.asyncIterator]();
	expect((await events.next()).value?.kind).toBe("start");
	expect((await events.next()).value?.kind).toBe("end");
	// A close drops what was queued and ends the iteration.
	producer.close();
	expect((await events.next()).done).toBe(true);
});

test("a same-name re-announce is a distinct update", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	// The stream is a log, not a set: it carries a redundant announce as its own update rather
	// than collapsing it. Deciding what a repeat means belongs to the session layer, which resolves
	// a restart into either nothing (an identical route) or an in-place update.
	const route = Route.default;
	producer.append({ prefix: p("a"), captures: undefined, kind: "start", route });
	producer.append({ prefix: p("a"), captures: undefined, kind: "start", route });

	expect(await consumer.next()).toEqual({ prefix: p("a"), captures: undefined, kind: "start", route });
	expect(await consumer.next()).toEqual({ prefix: p("a"), captures: undefined, kind: "start", route });
});

test("closing resolves next with undefined", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.close();
	expect(await consumer.next()).toBeUndefined();
});

test("closing a consumer releases only that handle", async () => {
	const producer = new Announce.Producer();
	expect(producer.wanted.peek()).toBe(false);
	const first = producer.consume();
	const second = producer.consume();
	expect(producer.wanted.peek()).toBe(true);

	// A reader parked on next() ends when its own handle closes, while another still wants it.
	const early = first.next();
	first.close();
	first.close();
	expect(await early).toBeUndefined();
	expect(await first.next()).toBeUndefined();
	expect(producer.wanted.peek()).toBe(true);
	expect(producer.closed.peek()).toBeUndefined();

	// The other reader keeps the stream: an event still reaches it.
	const route = Route.default;
	producer.append({ prefix: p("a"), captures: undefined, kind: "start", route });
	expect((await second.next())?.prefix).toBe(p("a"));

	// The last one leaving is what the producer watches for.
	const unwanted = producer.wanted.changed();
	second.close();
	expect(await unwanted).toBe(false);
	expect(producer.closed.peek()).toBeUndefined();
	producer.close();
});

test("aborting rejects next", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.close(new Error("boom"));
	await expect(consumer.next()).rejects.toThrow("boom");
});

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

test("an origin handle resolves a local publish with no session attached", async () => {
	const origin = new OriginProducer();
	const path = p("loopback");

	const watch = origin.follow(path);
	await settle();
	expect(watch.active.peek()).toBeUndefined();

	// Loopback needs no connection: the table routes the local publish directly, even
	// though `discovery` is still undefined (nothing is attached).
	const first = publish(origin, path);
	await settle();
	const held = watch.active.peek();
	expect(held).toBeDefined();

	// The same request moves to the new publisher instance.
	const second = publish(origin, path);
	await settle();
	expect(watch.active.peek()).toBeDefined();
	expect(watch.active.peek()).not.toBe(held);
	expect(watch.closed.peek()).toBeUndefined();

	// Unpublishing clears active while keeping the request open for another start.
	second.close();
	first.close();
	await settle();
	expect(watch.active.peek()).toBeUndefined();
	expect(watch.closed.peek()).toBeUndefined();

	watch.close();
	origin.close();
});

test("the local route wins over a blind request on a no-discovery origin", async () => {
	const origin = new OriginProducer();
	const path = p("local-first");

	// A session without discovery is attached, so the handle stands a request; but the
	// local publish must still resolve through the table, not wait on an answer.
	const detach = wireOf(origin).attach(false);
	const broadcast = publish(origin, path);

	const watch = origin.follow(path);
	await settle();
	expect(watch.active.peek()).toBeDefined();

	watch.close();
	detach();
	broadcast.close();
	origin.close();
});

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const start = (prefix: string): Announce.Event => ({
	prefix: p(prefix),
	captures: undefined,
	kind: "start",
	route: Route.default,
});

test("a stream syncs once the reader is past what was queued at the boundary", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();
	producer.append(start("a"));
	producer.append(start("b"));
	expect(consumer.synced.peek()).toBe(false);

	producer.synced();
	// Appended after the boundary, so not owed.
	producer.append(start("c"));
	expect(consumer.synced.peek()).toBe(false);

	await consumer.next();
	await consumer.next();
	// Still handling b.
	expect(consumer.synced.peek()).toBe(false);
	expect(await consumer.next()).toMatchObject({ prefix: p("c") });
	expect(consumer.synced.peek()).toBe(true);
});

test("an event taken before the boundary is handled before syncing", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();
	producer.append(start("a"));
	await consumer.next();
	producer.synced();
	expect(consumer.synced.peek()).toBe(false);

	void consumer.next();
	expect(consumer.synced.peek()).toBe(true);
	producer.close();
});

test("behind takes sync back, and closing hands it back for good", () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();
	producer.synced();
	expect(consumer.synced.peek()).toBe(true);
	producer.behind();
	expect(consumer.synced.peek()).toBe(false);
	producer.close();
	expect(consumer.synced.peek()).toBe(true);
	producer.behind();
	expect(consumer.synced.peek()).toBe(true);
});

test("an origin with no session delivering syncs on its own replay", async () => {
	const origin = new OriginProducer();
	const room = publish(origin, p("room"));
	const announced = origin.consume().announced();
	expect(await announced.next()).toMatchObject({ prefix: p("room"), kind: "start" });
	void announced.next();
	await tick();
	expect(announced.synced.peek()).toBe(true);
	room.close();
	announced.close();
});

test("an origin stream waits on every overlapping initial set in flight", async () => {
	const origin = new OriginProducer();
	const wire = wireOf(origin);
	const room = wire.snapshot(p("room"));
	const other = wire.snapshot(p("other"));

	const scoped = origin.consume().announced(Path.Pattern.subtree(p("room")));
	const all = origin.consume().announced();
	await tick();
	expect(scoped.synced.peek()).toBe(false);
	expect(all.synced.peek()).toBe(false);

	other();
	await tick();
	// Still held by the room snapshot; the other one never held the scoped stream.
	expect(scoped.synced.peek()).toBe(false);
	expect(all.synced.peek()).toBe(false);

	// The initial set lands, and so does its snapshot.
	const route = wire.receive(p("room/cam"));
	room();
	expect(await scoped.next()).toMatchObject({ prefix: p("room/cam"), kind: "start" });
	void scoped.next();
	await tick();
	expect(scoped.synced.peek()).toBe(true);

	// A new session, as a reconnect makes, takes it back until its own set lands.
	const second = wire.snapshot(p(""));
	await tick();
	expect(scoped.synced.peek()).toBe(false);
	second();
	await tick();
	expect(scoped.synced.peek()).toBe(true);

	route.close();
	scoped.close();
	all.close();
});

test("a snapshot elsewhere never holds a scoped stream", async () => {
	const origin = new OriginProducer();
	const other = wireOf(origin).snapshot(p("other"));
	const scoped = origin.consume().announced(Path.Pattern.subtree(p("room")));
	await tick();
	expect(scoped.synced.peek()).toBe(true);
	other();
	scoped.close();
});

test("a snapshot under a hidden segment holds only a reader that opts in", async () => {
	const origin = new OriginProducer();
	const stats = wireOf(origin).snapshot(p(".stats"));
	const all = origin.consume().announced();
	const hidden = origin.consume().announced(Path.Pattern.all(), { hidden: true });
	await tick();
	expect(all.synced.peek()).toBe(true);
	expect(hidden.synced.peek()).toBe(false);
	stats();
	all.close();
	hidden.close();
});

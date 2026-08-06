import { expect, test } from "bun:test";
import * as Announce from "./announced.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";

const p = (s: string) => Path.from(s);

test("next streams every appended event in order", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.append({ path: p("a"), active: true });
	producer.append({ path: p("a"), active: false });

	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
	expect(await consumer.next()).toEqual({ path: p("a"), active: false });
});

test("a same-name re-announce is a distinct update", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	// The stream is a log, not a set: it carries a redundant active:true as its own update rather
	// than collapsing it. Deciding what a repeat means belongs to the session layer, which resolves
	// a restart into either nothing (a route change) or an end + start (a new publisher).
	producer.append({ path: p("a"), active: true });
	producer.append({ path: p("a"), active: true });

	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
	expect(await consumer.next()).toEqual({ path: p("a"), active: true });
});

test("closing resolves next with undefined", async () => {
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	producer.close();
	expect(await consumer.next()).toBeUndefined();
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

	const watch = new Announce.Broadcast({ origin, path });
	await settle();
	expect(watch.active.peek()).toBeUndefined();

	// Loopback needs no connection: the table routes the local publish directly, even
	// though `discovery` is still undefined (nothing is attached).
	const first = origin.publish(path);
	await settle();
	const held = watch.active.peek();
	expect(held).toBeDefined();

	// A republish swaps the handle to the new broadcast rather than clinging to the
	// superseded one.
	const second = origin.publish(path);
	await settle();
	expect(watch.active.peek()).toBeDefined();
	expect(watch.active.peek()).not.toBe(held);

	// Unpublishing takes it offline.
	second.close();
	first.close();
	await settle();
	expect(watch.active.peek()).toBeUndefined();

	watch.close();
	origin.close();
});

test("the local route wins over a blind request on a no-discovery origin", async () => {
	const origin = new OriginProducer();
	const path = p("local-first");

	// A session without discovery is attached, so the handle stands a request; but the
	// local publish must still resolve through the table, not wait on an answer.
	const detach = origin.attach(false);
	const broadcast = origin.publish(path);

	const watch = new Announce.Broadcast({ origin, path });
	await settle();
	expect(watch.active.peek()).toBeDefined();

	watch.close();
	detach();
	broadcast.close();
	origin.close();
});

test("BroadcastProps requires a source", () => {
	// @ts-expect-error neither source is a compile error; the union demands exactly one.
	const neither: Announce.BroadcastProps = { path: p("x") };
	expect(neither.path).toBeDefined();
});

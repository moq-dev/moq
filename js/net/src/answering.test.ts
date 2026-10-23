import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { Broadcast, Origin, Path } from "./index.ts";

beforeEach(() => jest.useFakeTimers());
afterEach(() => jest.useRealTimers());

async function settle() {
	for (let i = 0; i < 64; i++) await Promise.resolve();
}

test("public track answering is armed before demand and stops without closing accepted tracks", async () => {
	const source = new Broadcast.Producer();
	const answering = source.dynamic();
	const consumer = source.consume();
	const subscribed = consumer.track("video").subscribe();
	const request = await answering.requestedTrack();
	expect(request?.name).toBe("video");
	const track = request?.accept();
	if (!track) throw new Error("expected track");
	const pending = answering.requestedTrack();
	answering.close();
	expect(await pending).toBeUndefined();
	expect(track.closed.peek()).toBeUndefined();
	const absent = consumer.track("other").subscribe();
	expect(absent.closed.peek()).toBeInstanceOf(Error);
	absent.close();
	subscribed.close();
	consumer.close();
	source.close();
});

test("a fallback is scoped, never advertised, and its answered requests end with it", async () => {
	const origin = new Origin.Producer();
	const allowed = new Path.Patterns([Path.Pattern.subtree(Path.from("room"))]);
	const fallback = origin.scope(Path.empty(), allowed).fallback();
	const iterator = fallback.requested();
	const unseen = origin.request(Path.from("elsewhere"));
	const wanted = origin.request(Path.from("room/alice"));
	const next = await iterator.next();
	expect(next.value?.path).toBe(Path.from("room/alice"));
	const source = new Broadcast.Producer();
	next.value?.accept(source);
	await settle();
	expect(wanted.active.peek()).toBeDefined();
	expect(unseen.active.peek()).toBeUndefined();
	const announcements = origin.announced();
	let advertised = false;
	const event = announcements.next().then((value) => {
		advertised = value !== undefined;
	});
	await settle();
	expect(advertised).toBe(false);
	fallback.close();
	await settle();
	expect(wanted.closed.peek()).toBeInstanceOf(Error);
	wanted.close();
	unseen.close();
	announcements.close();
	await event;
	source.close();
	origin.close();
	await iterator.return?.(undefined);
});

test("an announced route retires a blind answer rather than stitching its request", async () => {
	const origin = new Origin.Producer();
	const fallback = origin.fallback();
	const iterator = fallback.requested();
	const wanted = origin.request(Path.from("room"));
	const next = await iterator.next();
	const blind = new Broadcast.Producer();
	next.value?.accept(blind);
	await settle();
	expect(wanted.active.peek()).toBeDefined();
	const announced = origin.createBroadcast(Path.from("room"));
	announced.announce();
	await settle();
	expect(wanted.closed.peek()).toBeInstanceOf(Error);
	const fresh = origin.request(Path.from("room"));
	expect(fresh.active.peek()).toBeDefined();
	fresh.close();
	wanted.close();
	fallback.close();
	announced.close();
	blind.close();
	origin.close();
	await iterator.return?.(undefined);
});

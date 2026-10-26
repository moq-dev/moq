import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast";
import * as Epoch from "./epoch";
import { StreamCode, StreamError } from "./error";
import { Route } from "./hop";
import { hooks } from "./internal.ts";
import { Producer, type Publication, type Request } from "./origin";
import * as Path from "./path";
import { wireOf } from "./wire";

const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

test("an announced request follows restart, end and a later start", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const firstPublication = origin.createBroadcast(path);
	const first = firstPublication.broadcast;
	firstPublication.announce();
	const request = origin.request(path, { announced: true });
	expect(request.active.peek()?.closed).toBe(first.closed);
	const secondPublication = origin.createBroadcast(path);
	const second = secondPublication.broadcast;
	secondPublication.announce();
	await settle();
	expect(request.active.peek()?.closed).toBe(second.closed);
	second.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeUndefined();
	const thirdPublication = origin.createBroadcast(path);
	const third = thirdPublication.broadcast;
	thirdPublication.announce();
	await settle();
	expect(request.active.peek()?.closed).toBe(third.closed);
	request.close();
	third.close();
	first.close();
	origin.close();
});

test("an epoch-pinned announced request refuses another instance", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const epoch = Epoch.mint();
	const firstPublication = origin.createBroadcast(path);
	const first = firstPublication.broadcast;
	firstPublication.announce({ epoch });
	const request = origin.request(path, { announced: true, epoch });
	const secondPublication = origin.createBroadcast(path);
	const second = secondPublication.broadcast;
	secondPublication.announce({ epoch: Epoch.mint() });
	await settle();
	expect(request.active.peek()?.closed).not.toBe(second.closed);
	request.close();
	first.close();
	second.close();
	origin.close();
});

test("an announced request changes its handle on an in-place epoch restart", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	const before = Epoch.mint();
	broadcastPublication.announce({ epoch: before });
	const request = origin.request(path, { announced: true });
	const old = request.active.peek();
	const after = Epoch.mint();
	broadcastPublication.announce({ epoch: after });
	await settle();
	expect(request.active.peek()?.epoch).toBe(after);
	expect(request.active.peek()).not.toBe(old);
	request.close();
	broadcast.close();
	origin.close();
});

test("a closed announced request never acquires another publisher", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const request = origin.request(path, { announced: true });
	request.close();
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeNull();
	broadcast.close();
	origin.close();
});

test("announced requests refuse paths that cannot be watched or are outside scope", () => {
	const origin = new Producer();
	expect(() => origin.request(Path.from("camera*main"), { announced: true })).toThrow();
	const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.subtree(Path.from("pool/job/cam"))]));
	expect(() => scoped.request(Path.from("pool/job"), { announced: true })).toThrow();
	origin.close();
});

test("unrelated routes never notify announced request followers", async () => {
	const origin = new Producer();
	const requests = Array.from({ length: 32 }, (_, i) => origin.request(Path.from(`room/${i}`), { announced: true }));
	await settle();
	let changes = 0;
	const stops = requests.map((request) => request.active.subscribe(() => changes++));
	const otherPublication = origin.createBroadcast(Path.from("unrelated"));
	const other = otherPublication.broadcast;
	otherPublication.announce();
	await settle();
	otherPublication.announce({ cost: 5n });
	await settle();
	other.close();
	await settle();
	expect(changes).toBe(0);
	for (const stop of stops) stop();
	for (const request of requests) request.close();
	origin.close();
});

test("an origin closing settles pending announced requests", async () => {
	const origin = new Producer();
	const request = origin.request(Path.from("absent"), { announced: true });
	origin.close();
	await settle();
	expect(request.closed.peek()).toBeNull();
	expect(request.active.peek()).toBeUndefined();
	request.close();
});

test("an announced request keeps its fallback blind answer", async () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(false);
	const path = Path.from("blind");
	const request = origin.request(path, { announced: true });
	const broadcast = new BroadcastProducer();
	const withdraw = wire.answer(path, broadcast.consume());
	await settle();
	expect(request.active.peek()?.closed).toBe(broadcast.closed);
	request.close();
	withdraw?.();
	detach();
	broadcast.close();
	origin.close();
});

test("announced requests follow the serving prefix, ignoring descendants and re-prices", async () => {
	const origin = new Producer();
	const source = new BroadcastProducer();
	const keeper = source.consume();
	const prefix = origin.dynamicRoute(Path.from("pool"));
	const serving = (async () => {
		for await (const request of prefix.requested()) hooks.acceptRequest(request, source.consume());
	})();
	const request = origin.request(Path.from("pool/job"), { announced: true });
	await settle();
	expect(request.active.peek()?.closed).toBe(source.closed);
	const belowPublication = origin.createBroadcast(Path.from("pool/job/thumb"));
	const below = belowPublication.broadcast;
	belowPublication.announce();
	await settle();
	expect(request.active.peek()?.closed).toBe(source.closed);
	const exactPublication = origin.createBroadcast(Path.from("pool/job"));
	const exact = exactPublication.broadcast;
	exactPublication.announce();
	await settle();
	expect(request.active.peek()?.closed).toBe(exact.closed);
	prefix.updateRoute({ cost: 5n });
	await settle();
	expect(request.active.peek()?.closed).toBe(exact.closed);
	exact.close();
	await settle();
	expect(request.active.peek()?.closed).toBe(source.closed);
	prefix.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeUndefined();
	request.close();
	below.close();
	keeper.close();
	source.close();
	origin.close();
	await serving;
});

test("an announced request ignores a covering route whose scope excludes its path", async () => {
	const origin = new Producer();
	const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/*/chat")]));
	const prefix = scoped.dynamicRoute(Path.from("room/alice"));
	const exactPublication = origin.createBroadcast(Path.from("room/alice"));
	const exact = exactPublication.broadcast;
	exactPublication.announce({ cost: 5n });
	const request = origin.request(Path.from("room/alice"), { announced: true });
	expect(request.active.peek()?.closed).toBe(exact.closed);
	exact.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	request.close();
	prefix.close();
	origin.close();
});

test("an announced request follows an epochless route replaced by an identical one", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const serve = (source: BroadcastProducer) => {
		const route = origin.dynamicRoute(path);
		void (async () => {
			for await (const request of route.requested()) hooks.acceptRequest(request, source.consume());
		})();
		return route;
	};
	const firstSource = new BroadcastProducer();
	const first = serve(firstSource);
	const request = origin.request(path, { announced: true });
	await settle();
	expect(request.active.peek()?.closed).toBe(firstSource.closed);

	// Same (default) route metadata and no epoch: only the entry's identity differs.
	const secondSource = new BroadcastProducer();
	const second = serve(secondSource);
	await settle();
	first.close();
	await settle();
	expect(request.active.peek()?.closed).toBe(secondSource.closed);

	request.close();
	second.close();
	firstSource.close();
	secondSource.close();
	origin.close();
});

test("an origin closing settles an announced request a served route resolved", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const source = new BroadcastProducer();
	const route = origin.dynamicRoute(path);
	void (async () => {
		for await (const request of route.requested()) hooks.acceptRequest(request, source.consume());
	})();
	const request = origin.request(path, { announced: true });
	await settle();
	expect(request.active.peek()).toBeDefined();
	const abort = new Error("shutdown");
	origin.close(abort);
	await settle();
	expect(request.closed.peek()).toBe(abort);
	expect(request.active.peek()).toBeUndefined();
	request.close();
	source.close();
});

test("an announced request keeps a refusal that lands with a re-price", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const route = origin.dynamicRoute(path);
	const requested = route.requested();
	const request = origin.request(path, { announced: true });
	const pending = (await requested.next()).value;
	if (!pending) throw new Error("expected a request");

	// Both land in one turn: the watch notification must not retry the refused request.
	route.updateRoute({ cost: 5n });
	const denied = new Error("denied");
	pending.reject(denied);
	await settle();
	expect(request.closed.peek()).toBe(denied);

	let asked = false;
	void requested.next().then(() => {
		asked = true;
	});
	await settle();
	expect(asked).toBe(false);
	request.close();
	route.close();
	origin.close();
});

for (const received of [false, true]) {
	test(`a plain request watches its path only through a received route (received: ${received})`, async () => {
		const origin = new Producer();
		const path = Path.from("room");
		const events: string[] = [];
		const stop = wireOf(origin).interests((prefix, wanted) => events.push(`${prefix} ${wanted}`));
		const source = new BroadcastProducer();
		const route = received ? wireOf(origin).receive(path, Route.default) : origin.dynamicRoute(path);
		void (async () => {
			for await (const request of route.requested()) hooks.acceptRequest(request, source.consume());
		})();

		const request = origin.request(path);
		await settle();
		expect(request.active.peek()).toBeDefined();
		// A local route needs no discovery; one a session received stays discovered while held.
		expect(events).toEqual(received ? ["room true"] : []);

		request.close();
		await settle();
		expect(events).toEqual(received ? ["room true", "room false"] : []);
		stop();
		route.close();
		source.close();
		origin.close();
	});
}

test("a claim answer is hidden until published under its own epoch", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const epoch = Epoch.mint();
	const prepared = pending.prepare(epoch);
	prepared.broadcast.createTrack("video");
	await settle();
	expect(request.active.peek()).toBeUndefined();
	const published = prepared.publish();
	await settle();
	expect(request.active.peek()?.epoch).toBe(epoch);
	request.close();
	published.broadcast.close();
	claim.close();
	origin.close();
});

test("a prepared answer cannot announce itself before publish", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const epoch = Epoch.mint();
	const prepared = pending.prepare(epoch);
	expect(prepared.broadcast).not.toHaveProperty("announce");
	expect(prepared.broadcast).not.toHaveProperty("route");
	await settle();
	expect(request.active.peek()).toBeUndefined();
	const published = prepared.publish();
	expect(published.route?.epoch).toBe(epoch);
	await settle();
	expect(request.active.peek()?.epoch).toBe(epoch);
	request.close();
	published.broadcast.close();
	claim.close();
	origin.close();
});

test("an application claim cannot accept an arbitrary broadcast", async () => {
	const origin = new Producer();
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending: Request | undefined = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	// @ts-expect-error Only sessions accept a request; an application prepares and publishes.
	expect(pending.accept).toBeUndefined();
	request.close();
	claim.close();
	origin.close();
});

test("closing a prepared answer refuses the request and closes its broadcast", async () => {
	const origin = new Producer();
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const prepared = pending.prepare(Epoch.mint());
	prepared.close();
	prepared.close();
	prepared[Symbol.dispose]();
	await settle();
	expect(request.closed.peek()).toBeInstanceOf(Error);
	expect(prepared.broadcast.closed.peek()).toBeDefined();
	claim.close();
	origin.close();
});

test("an answer prepared for an abandoned claim is never announced", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const prepared = pending.prepare(Epoch.mint());
	claim.close();
	expect(() => prepared.publish()).toThrow("request is closed");
	const announced = origin.request(path, { announced: true });
	await settle();
	expect(announced.active.peek()).toBeUndefined();
	announced.close();
	request.close();
	origin.close();
});

for (const cleanup of ["close", "dispose", "broadcast"] as const) {
	test(`prepared ${cleanup} releases shared requesters and admits a new request`, async () => {
		const origin = new Producer();
		const path = Path.from("room/alice");
		const claim = origin.dynamic(Path.from("room"));
		const requests = claim.requested();
		const first = origin.request(path);
		const pending = (await requests.next()).value;
		if (!pending) throw new Error("expected a request");
		const prepared = pending.prepare(Epoch.mint());
		const shared = origin.request(path);
		if (cleanup === "close") prepared.close();
		else if (cleanup === "dispose") prepared[Symbol.dispose]();
		else prepared.broadcast.close();
		// Requested in the same turn, so the release cannot wait on a reactive notification.
		const later = origin.request(path);
		await settle();
		prepared.close();
		prepared[Symbol.dispose]();

		for (const request of [first, shared]) {
			expect(request.closed.peek()).toBeInstanceOf(StreamError);
			expect((request.closed.peek() as StreamError).code).toBe(StreamCode.Unroutable);
		}
		expect(later.closed.peek()).toBeUndefined();
		expect(() => prepared.publish()).toThrow("prepared answer is closed");
		const next = (await requests.next()).value;
		if (!next) throw new Error("expected another request");
		expect(next).not.toBe(pending);
		next.reject(new Error("done"));
		await settle();
		expect(later.closed.peek()).toBeInstanceOf(Error);
		claim.close();
		origin.close();
	});
}

test("a throwing handler disposes its prepared answer and leaves the claim answerable", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.dynamic(Path.from("room"));
	const failed = (async () => {
		for await (const request of claim.requested()) {
			using prepared = request.prepare(Epoch.mint());
			prepared.broadcast.createTrack("video");
			throw new Error("handler failed");
		}
	})();
	const first = origin.request(path);
	await expect(failed).rejects.toThrow("handler failed");
	await settle();
	expect((first.closed.peek() as StreamError).code).toBe(StreamCode.Unroutable);
	const requests = claim.requested();
	const later = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected another request");
	{
		using prepared = pending.prepare(Epoch.mint());
		prepared.close();
	}
	await settle();
	expect((later.closed.peek() as StreamError).code).toBe(StreamCode.Unroutable);
	claim.close();
	origin.close();
});

test("using transfers a published answer without closing it on disposal", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	let published: Publication;
	{
		using prepared = pending.prepare(Epoch.mint());
		prepared.broadcast.createTrack("data").writeFrame({ payload: new TextEncoder().encode("published") });
		published = prepared.publish();
		expect(() => prepared.publish()).toThrow("prepared answer is closed");
		prepared.close();
	}
	await settle();
	expect(published.broadcast.closed.peek()).toBeUndefined();
	expect(request.active.peek()?.epoch).toBe(published.route?.epoch);
	const reader = request.active.peek()?.track("data").subscribe().ordered();
	expect(await reader?.readString()).toBe("published");
	reader?.close();
	request.close();
	published.broadcast.close();
	claim.close();
	origin.close();
});

for (const cleanup of ["close", "dispose", "broadcast", "publish"] as const) {
	test(`an old prepared ${cleanup} cannot refuse the next request for its path`, async () => {
		const origin = new Producer();
		const path = Path.from("room/alice");
		const claim = origin.dynamic(Path.from("room"));
		const requests = claim.requested();
		const first = origin.request(path);
		const pending = (await requests.next()).value;
		if (!pending) throw new Error("expected a request");
		const old = pending.prepare(Epoch.mint());
		first.close();
		await settle();
		const later = origin.request(path);
		const next = (await requests.next()).value;
		if (!next) throw new Error("expected another request");
		if (cleanup === "close") old.close();
		else if (cleanup === "dispose") old[Symbol.dispose]();
		else if (cleanup === "broadcast") old.broadcast.close();
		else expect(() => old.publish()).toThrow("request is closed");
		await settle();
		expect(later.closed.peek()).toBeUndefined();
		using prepared = next.prepare(Epoch.mint());
		prepared.broadcast.createTrack("data").writeFrame({ payload: new TextEncoder().encode(cleanup) });
		const published = prepared.publish();
		await settle();
		expect(later.active.peek()?.epoch).toBe(published.route?.epoch);
		const reader = later.active.peek()?.track("data").subscribe().ordered();
		expect(await reader?.readString()).toBe(cleanup);
		reader?.close();
		old.close();
		later.close();
		published.broadcast.close();
		claim.close();
		origin.close();
	});
}

test("a forwarded request cannot be prepared by setting a publisher property", async () => {
	const origin = new Producer();
	const route = origin.dynamicRoute(Path.from("room"));
	const requests = route.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	Object.assign(pending, { publisher: origin });
	expect(() => pending.prepare(Epoch.mint())).toThrow("forwarded requests cannot be prepared");
	pending.reject(new Error("refused"));
	request.close();
	route.close();
	origin.close();
});

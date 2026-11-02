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

test("a follower moves through restart, end and a later start", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const firstPublication = origin.createBroadcast(path);
	const first = firstPublication.broadcast;
	firstPublication.announce();
	const request = origin.follow(path);
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

test("an epoch-pinned follower refuses another instance", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const epoch = Epoch.mint();
	const firstPublication = origin.createBroadcast(path);
	const first = firstPublication.broadcast;
	firstPublication.announce({ epoch });
	const request = origin.follow(path, { epoch });
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

test("a follower changes its handle on an in-place epoch restart", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	const before = Epoch.mint();
	broadcastPublication.announce({ epoch: before });
	const request = origin.follow(path);
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

test("a closed follower never acquires another publisher", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const request = origin.follow(path);
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

test.each(["request", "follow"] as const)(
	"%s refuses unwatchable paths and scope violations before registering",
	(method) => {
		const origin = new Producer();
		expect(() => origin[method](Path.from("camera*main"))).toThrow();
		const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.subtree(Path.from("pool/job/cam"))]));
		expect(() => scoped[method](Path.from("pool/job"))).toThrow();
		expect(wireOf(origin).requests.peek()?.size).toBe(0);
		origin.close();
	},
);

test("unrelated routes never notify followers", async () => {
	const origin = new Producer();
	const requests = Array.from({ length: 32 }, (_, i) => origin.follow(Path.from(`room/${i}`)));
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

test("an origin closing settles pending followers", async () => {
	const origin = new Producer();
	const request = origin.follow(Path.from("absent"));
	origin.close();
	await settle();
	expect(request.closed.peek()).toBeNull();
	expect(request.active.peek()).toBeUndefined();
	request.close();
});

test("only a request can use a session without discovery", async () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(false);
	const path = Path.from("blind");
	const followed = origin.follow(path);
	const slot = wire.requests.peek()?.get(path);
	if (!slot) throw new Error("request did not register");
	expect(wire.blind(slot)).toBe(false);
	const requested = origin.request(path);
	expect(wire.blind(slot)).toBe(true);
	// The blind answer lands in the shared slot, but only the plain handle takes it.
	const source = new BroadcastProducer();
	const withdraw = wire.answer(path, source.consume());
	await settle();
	expect(requested.active.peek()?.closed).toBe(source.closed);
	expect(followed.active.peek()).toBeUndefined();
	const joined = origin.follow(path);
	await settle();
	expect(joined.active.peek()).toBeUndefined();
	joined.close();
	requested.close();
	await settle();
	expect(wire.blind(slot)).toBe(false);
	followed.close();
	withdraw?.();
	source.close();
	detach();
	origin.close();
});

test("followers track the serving prefix, ignoring descendants and re-prices", async () => {
	const origin = new Producer();
	const source = new BroadcastProducer();
	const keeper = source.consume();
	const prefix = origin.requestsRoute(Path.from("pool"));
	const serving = (async () => {
		for await (const request of prefix.requested()) hooks.acceptRequest(request, source.consume());
	})();
	const request = origin.follow(Path.from("pool/job"));
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

test("a follower ignores a covering route whose scope excludes its path", async () => {
	const origin = new Producer();
	const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/*/chat")]));
	const prefix = scoped.requestsRoute(Path.from("room/alice"));
	const exactPublication = origin.createBroadcast(Path.from("room/alice"));
	const exact = exactPublication.broadcast;
	exactPublication.announce({ cost: 5n });
	const request = origin.follow(Path.from("room/alice"));
	expect(request.active.peek()?.closed).toBe(exact.closed);
	exact.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	request.close();
	prefix.close();
	origin.close();
});

test("a follower tracks an epochless route replaced by an identical one", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const serve = (source: BroadcastProducer) => {
		const route = origin.requestsRoute(path);
		void (async () => {
			for await (const request of route.requested()) hooks.acceptRequest(request, source.consume());
		})();
		return route;
	};
	const firstSource = new BroadcastProducer();
	const first = serve(firstSource);
	const request = origin.follow(path);
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

test("an origin closing settles a follower a served route resolved", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const source = new BroadcastProducer();
	const route = origin.requestsRoute(path);
	void (async () => {
		for await (const request of route.requested()) hooks.acceptRequest(request, source.consume());
	})();
	const request = origin.follow(path);
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

test("a follower keeps a refusal that lands with a re-price", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const route = origin.requestsRoute(path);
	const requested = route.requested();
	const request = origin.follow(path);
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
	test(`a request owns discovery through both local and received routes (received: ${received})`, async () => {
		const origin = new Producer();
		const path = Path.from("room");
		const events: string[] = [];
		const stop = wireOf(origin).interests((prefix, wanted) => events.push(`${prefix} ${wanted}`));
		const source = new BroadcastProducer();
		const route = received ? wireOf(origin).receive(path, Route.default) : origin.requestsRoute(path);
		void (async () => {
			for await (const request of route.requested()) hooks.acceptRequest(request, source.consume());
		})();

		const request = origin.request(path);
		await settle();
		expect(request.active.peek()).toBeDefined();
		// Every request owns discovery, independent of the route that currently serves it.
		expect(events).toEqual(["room true"]);

		request.close();
		await settle();
		expect(events).toEqual(["room true", "room false"]);
		stop();
		route.close();
		source.close();
		origin.close();
	});
}

test("a claim answer is hidden until published under its own epoch", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.requests(Path.from("room"));
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
	const claim = origin.requests(Path.from("room"));
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
	const claim = origin.requests(Path.from("room"));
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
	const claim = origin.requests(Path.from("room"));
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
	const claim = origin.requests(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const prepared = pending.prepare(Epoch.mint());
	claim.close();
	expect(() => prepared.publish()).toThrow("request is closed");
	const announced = origin.follow(path);
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
		const claim = origin.requests(Path.from("room"));
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
	const claim = origin.requests(Path.from("room"));
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
	const claim = origin.requests(Path.from("room"));
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
		const claim = origin.requests(Path.from("room"));
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
	const route = origin.requestsRoute(Path.from("room"));
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

test("an epoch-pinned follower resumes when its instance returns after a gap", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const epoch = Epoch.mint();
	const source = new BroadcastProducer();
	const keeper = source.consume();
	const serve = (instance: Epoch.Valid, broadcast: BroadcastProducer) => {
		const route = origin.requestsRoute(path, { epoch: instance });
		void (async () => {
			for await (const request of route.requested()) hooks.acceptRequest(request, broadcast.consume());
		})();
		return route;
	};
	const first = serve(epoch, source);
	const request = origin.follow(path, { epoch });
	await settle();
	expect(request.active.peek()?.epoch).toBe(epoch);
	first.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeUndefined();
	const otherSource = new BroadcastProducer();
	const other = serve(Epoch.mint(), otherSource);
	await settle();
	expect(request.active.peek()).toBeUndefined();
	other.close();
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	const back = serve(epoch, source);
	await settle();
	expect(request.active.peek()?.epoch).toBe(epoch);
	request.close();
	back.close();
	keeper.close();
	source.close();
	otherSource.close();
	origin.close();
});

test("a request owns its scoped discovery until closed", async () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(true);
	const path = Path.from("room/audio");
	const interests: string[] = [];
	let synced: (() => void) | undefined;
	const stop = wire.interests((prefix, wanted) => {
		interests.push(`${prefix} ${wanted}`);
		if (wanted) synced = wire.snapshot(prefix);
	});
	const request = origin.request(path);
	expect(interests).toEqual(["room/audio true"]);
	synced?.();
	expect(request.closed.peek()).toBeUndefined();
	request.close();
	expect(interests).toEqual(["room/audio true", "room/audio false"]);
	stop();
	detach();
	origin.close();
});

test("a follower keeps its scoped discovery across instances until closed", async () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(true);
	const path = Path.from("room/audio");
	const interests: string[] = [];
	let synced: (() => void) | undefined;
	const stop = wire.interests((prefix, wanted) => {
		interests.push(`${prefix} ${wanted}`);
		if (wanted) synced = wire.snapshot(prefix);
	});
	const follower = origin.follow(path);
	expect(interests).toEqual(["room/audio true"]);
	synced?.();
	await settle();
	expect(follower.closed.peek()).toBeUndefined();
	const publication = origin.createBroadcast(path);
	publication.announce();
	await settle();
	expect(follower.active.peek()?.closed).toBe(publication.broadcast.closed);
	publication.broadcast.close();
	await settle();
	// The instance ending keeps the follower and its discovery open for the next one.
	expect(follower.active.peek()).toBeUndefined();
	expect(follower.closed.peek()).toBeUndefined();
	expect(interests).toEqual(["room/audio true"]);
	follower.close();
	expect(interests).toEqual(["room/audio true", "room/audio false"]);
	stop();
	detach();
	origin.close();
});

test("a pending request fails Unroutable once its discovery syncs without a route", async () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(true);
	const initialSets: (() => void)[] = [];
	const stop = wire.interests((prefix, wanted) => {
		if (wanted) initialSets.push(wire.snapshot(prefix));
	});
	const missing = origin.request(Path.from("room/missing"));
	const found = origin.request(Path.from("room/found"));
	const pinned = origin.request(Path.from("room/pinned"), { epoch: Epoch.mint() });
	const follower = origin.follow(Path.from("room/followed"));
	// The initial set reports these before it lands, and nothing at all for the rest.
	const route = wire.receive(Path.from("room/found"));
	const other = wire.receive(Path.from("room/pinned"), { epoch: Epoch.mint() });
	await settle();
	for (const request of [missing, found, pinned, follower]) expect(request.closed.peek()).toBeUndefined();

	for (const synced of initialSets) synced();
	await settle();
	for (const request of [missing, pinned]) {
		const err = request.closed.peek();
		expect(err).toBeInstanceOf(StreamError);
		expect((err as StreamError).code).toBe(StreamCode.Unroutable);
	}
	expect(found.closed.peek()).toBeUndefined();
	expect(follower.closed.peek()).toBeUndefined();

	for (const request of [missing, found, pinned, follower]) request.close();
	route.close();
	other.close();
	stop();
	detach();
	origin.close();
});

test("a follower retains a selected handler's explicit unroutable refusal", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const route = origin.requestsRoute(path);
	const pending = route.requested();
	const requested = origin.follow(path);
	const request = (await pending.next()).value;
	if (!request) throw new Error("request did not reach its handler");
	const refused = new StreamError(StreamCode.Unroutable);
	request.reject(refused);
	await settle();
	expect(requested.closed.peek()).toBe(refused);
	requested.close();
	route.close();
	origin.close();
});

test("a pending request retries a route gap and never retargets after resolving", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const first = origin.requestsRoute(path);
	const requested = origin.request(path);
	const before = await first.requested().next();
	expect(before.value).toBeDefined();
	first.close();
	await settle();
	expect(requested.closed.peek()).toBeUndefined();
	const second = origin.requestsRoute(path);
	const pending = (await second.requested().next()).value;
	if (!pending) throw new Error("replacement route was not requested");
	const source = new BroadcastProducer();
	const keeper = source.consume();
	hooks.acceptRequest(pending, source.consume());
	await settle();
	expect(requested.active.peek()?.closed).toBe(source.closed);
	const replacement = origin.createBroadcast(path);
	replacement.announce();
	await settle();
	expect(requested.active.peek()?.closed).toBe(source.closed);
	second.close();
	await settle();
	expect(requested.closed.peek()).toBeInstanceOf(StreamError);
	expect(requested.active.peek()).toBeUndefined();
	requested.close();
	replacement.broadcast.close();
	keeper.close();
	source.close();
	origin.close();
});

test("a plain epoch pin never takes an answer without that epoch", () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(false);
	const path = Path.from("room/audio");
	const requested = origin.request(path, { epoch: Epoch.mint() });
	expect(requested.closed.peek()).toBeUndefined();
	const source = new BroadcastProducer();
	const withdraw = wire.answer(path, source.consume());
	expect(requested.active.peek()).toBeUndefined();
	expect(requested.closed.peek()).toBeUndefined();
	requested.close();
	withdraw?.();
	source.close();
	detach();
	origin.close();
});

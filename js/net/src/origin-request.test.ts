import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast";
import { StreamCode, StreamError } from "./error";
import { RouteInfo } from "./hop";
import * as Instance from "./instance.ts";
import { type Following, Producer, type Requesting } from "./origin";
import * as Path from "./path";
import { wireOf } from "./wire";

const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

test("a follower moves through restart, end and a later start", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const firstRoute = origin.route(path);
	const first = firstRoute.broadcast;
	const request: Following = origin.follow(path);
	expect(request.active.peek()?.instance).toBe(firstRoute.instance);
	const secondRoute = origin.route(path);
	const second = secondRoute.broadcast;
	await settle();
	expect(request.active.peek()?.instance).toBe(secondRoute.instance);
	second.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeUndefined();
	const thirdRoute = origin.route(path);
	const third = thirdRoute.broadcast;
	await settle();
	expect(request.active.peek()?.instance).toBe(thirdRoute.instance);
	request.close();
	third.close();
	first.close();
	origin.close();
});

test("an instance-pinned follower refuses another instance", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const instance = Instance.random();
	const firstRoute = origin.route(path, { instance });
	const first = firstRoute.broadcast;
	const request = origin.follow(path, { instance });
	const secondRoute = origin.route(path, { instance: Instance.random() });
	const second = secondRoute.broadcast;
	await settle();
	expect(request.active.peek()?.instance).not.toBe(secondRoute.instance);
	request.close();
	first.close();
	second.close();
	origin.close();
});

test("a follower changes its handle on an in-place instance restart", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const before = Instance.random();
	const broadcastRoute = origin.route(path, { instance: before });
	const broadcast = broadcastRoute.broadcast;
	const request = origin.follow(path);
	const old = request.active.peek();
	const after = Instance.random();
	// A session's in-place restart: the route's instance changes under it.
	wireOf(broadcastRoute).update({ instance: after });
	await settle();
	expect(request.active.peek()?.instance).toBe(after);
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
	const broadcastRoute = origin.route(path);
	const broadcast = broadcastRoute.broadcast;
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
	const otherRoute = origin.route(Path.from("unrelated"));
	const other = otherRoute.broadcast;
	await settle();
	otherRoute.setCost(5n);
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
	const requested: Requesting = origin.request(path);
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
	const prefix = origin.routePrefix(Path.from("pool"));
	const serving = (async () => {
		for await (const request of prefix.requested()) request.serve(source.consume());
	})();
	const request = origin.follow(Path.from("pool/job"));
	await settle();
	expect(request.active.peek()?.instance).toBe(prefix.instance);
	const belowRoute = origin.route(Path.from("pool/job/thumb"));
	const below = belowRoute.broadcast;
	await settle();
	expect(request.active.peek()?.instance).toBe(prefix.instance);
	const exactRoute = origin.route(Path.from("pool/job"));
	const exact = exactRoute.broadcast;
	await settle();
	expect(request.active.peek()?.instance).toBe(exactRoute.instance);
	prefix.setCost(5n);
	await settle();
	expect(request.active.peek()?.instance).toBe(exactRoute.instance);
	exact.close();
	await settle();
	expect(request.active.peek()?.instance).toBe(prefix.instance);
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
	const prefix = scoped.routePrefix(Path.from("room/alice"));
	const exactRoute = origin.route(Path.from("room/alice"), { cost: 5n });
	const exact = exactRoute.broadcast;
	const request = origin.follow(Path.from("room/alice"));
	expect(request.active.peek()?.instance).toBe(exactRoute.instance);
	exact.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	request.close();
	prefix.close();
	origin.close();
});

test("a follower tracks a route replaced by an identical one", async () => {
	const origin = new Producer();
	const path = Path.from("room");
	const serve = (source: BroadcastProducer) => {
		const route = origin.routePrefix(path);
		void (async () => {
			for await (const request of route.requested()) request.serve(source.consume());
		})();
		return route;
	};
	const firstSource = new BroadcastProducer();
	const first = serve(firstSource);
	const request = origin.follow(path);
	await settle();
	expect(request.active.peek()?.instance).toBe(first.instance);

	// Same (default) route metadata: only the instance each claim minted differs.
	const secondSource = new BroadcastProducer();
	const second = serve(secondSource);
	await settle();
	first.close();
	await settle();
	expect(request.active.peek()?.instance).toBe(second.instance);

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
	const route = origin.routePrefix(path);
	void (async () => {
		for await (const request of route.requested()) request.serve(source.consume());
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
	const route = origin.routePrefix(path);
	const requested = route.requested();
	const request = origin.follow(path);
	const pending = (await requested.next()).value;
	if (!pending) throw new Error("expected a request");

	// Both land in one turn: the watch notification must not retry the refused request.
	route.setCost(5n);
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
		const route = received ? wireOf(origin).receive(path, RouteInfo.default) : origin.routePrefix(path);
		void (async () => {
			for await (const request of route.requested()) request.serve(source.consume());
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

test("a hidden job route is hidden until announced under its own instance", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.routePrefix(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const job = pending.route({ hidden: true });
	job.broadcast.createTrack("video");
	await settle();
	expect(request.active.peek()).toBeUndefined();
	job.announce();
	await settle();
	expect(request.active.peek()?.instance).toBe(job.instance);
	request.close();
	job.close();
	claim.close();
	origin.close();
});

test("a visible job route answers its waiting requesters at once", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.routePrefix(Path.from("room"));
	const requests = claim.requested();
	const first = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const shared = origin.request(path);
	const job = pending.route();
	expect(job.announced).toBe(true);
	expect(job.instance).not.toBe(claim.instance);
	await settle();
	for (const request of [first, shared]) expect(request.active.peek()?.instance).toBe(job.instance);
	// The request is answered: routing it again is refused.
	expect(() => pending.route()).toThrow("request is closed");

	// Ending the job withdraws only its path: the claim keeps answering later requests.
	job.close();
	await settle();
	const later = origin.request(path);
	const next = (await requests.next()).value;
	if (!next) throw new Error("expected another request");
	const again = next.route({ instance: Instance.parse("stable"), epoch: 2n });
	await settle();
	expect(again.instance).toBe(Instance.parse("stable"));
	expect(later.active.peek()?.instance).toBe(again.instance);
	for (const request of [first, shared, later]) request.close();
	again.close();
	claim.close();
	origin.close();
});

test("a hidden job route's broadcast cannot announce itself", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.routePrefix(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const job = pending.route({ hidden: true });
	expect(job.broadcast).not.toHaveProperty("announce");
	expect(job.broadcast).not.toHaveProperty("route");
	await settle();
	expect(request.active.peek()).toBeUndefined();
	job.announce();
	await settle();
	expect(request.active.peek()?.instance).toBe(job.instance);
	request.close();
	job.close();
	claim.close();
	origin.close();
});

test("an application serves held content without an exact route", async () => {
	const origin = new Producer();
	const claim = origin.routePrefix(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const source = new BroadcastProducer();
	source.createTrack("data").writeFrame({ payload: new TextEncoder().encode("held") });
	pending.serve(source.consume());
	await settle();
	const reader = request.active.peek()?.track("data").subscribe().ordered();
	expect(await reader?.readString()).toBe("held");
	expect(wireOf(origin.consume()).advertised.peek()?.has(Path.from("room/alice"))).toBe(false);
	reader?.close();
	request.close();
	claim.close();
	source.close();
	origin.close();
});

for (const withdraw of ["withdraw", "replace"] as const) {
	test(`a job's route is not cached after ${withdraw}`, async () => {
		const origin = new Producer();
		const claim = origin.routePrefix(Path.from("room"));
		const requests = claim.requested();
		const path = Path.from("room/alice");
		const first = origin.request(path);
		const pending = (await requests.next()).value;
		if (!pending) throw new Error("expected a request");
		const job = pending.route();
		await settle();
		const held = first.active.peek()?.clone();
		if (withdraw === "withdraw") job.withdraw();
		else origin.route(path, { hidden: true }).broadcast.close();
		const next = origin.request(path);
		await settle();
		expect(next.active.peek()).toBeUndefined();
		const fresh = (await requests.next()).value;
		if (!fresh) throw new Error("expected another request");
		fresh.reject(new Error("no replacement"));
		held?.close();
		next.close();
		first.close();
		job.close();
		claim.close();
		origin.close();
	});
}

test("closing a hidden job route refuses the request and closes its broadcast", async () => {
	const origin = new Producer();
	const claim = origin.routePrefix(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const job = pending.route({ hidden: true });
	job.close();
	job.close();
	job[Symbol.dispose]();
	await settle();
	expect(request.closed.peek()).toBeInstanceOf(Error);
	expect(job.broadcast.closed.peek()).toBeDefined();
	claim.close();
	origin.close();
});

test("a job route for an abandoned claim is never announced", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.routePrefix(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const job = pending.route({ hidden: true });
	claim.close();
	expect(() => job.announce()).toThrow("unroutable");
	expect(job.announced).toBe(false);
	expect(job.broadcast.closed.peek()).toBeDefined();
	const announced = origin.follow(path);
	await settle();
	expect(announced.active.peek()).toBeUndefined();
	announced.close();
	request.close();
	origin.close();
});

for (const cleanup of ["close", "dispose", "broadcast"] as const) {
	test(`a hidden job route's ${cleanup} releases shared requesters and admits a new request`, async () => {
		const origin = new Producer();
		const path = Path.from("room/alice");
		const claim = origin.routePrefix(Path.from("room"));
		const requests = claim.requested();
		const first = origin.request(path);
		const pending = (await requests.next()).value;
		if (!pending) throw new Error("expected a request");
		const job = pending.route({ hidden: true });
		const shared = origin.request(path);
		if (cleanup === "close") job.close();
		else if (cleanup === "dispose") job[Symbol.dispose]();
		else job.broadcast.close();
		// Requested in the same turn, so the release cannot wait on a reactive notification.
		const later = origin.request(path);
		await settle();
		job.close();
		job[Symbol.dispose]();

		for (const request of [first, shared]) {
			expect(request.closed.peek()).toBeInstanceOf(StreamError);
			expect((request.closed.peek() as StreamError).code).toBe(StreamCode.Unroutable);
		}
		expect(later.closed.peek()).toBeUndefined();
		expect(() => job.announce()).toThrow("broadcast is closed");
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

test("a throwing handler disposes its job route and leaves the claim answerable", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.routePrefix(Path.from("room"));
	const failed = (async () => {
		for await (const request of claim.requested()) {
			using job = request.route({ hidden: true });
			job.broadcast.createTrack("video");
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
		using job = pending.route({ hidden: true });
		job.close();
	}
	await settle();
	expect((later.closed.peek() as StreamError).code).toBe(StreamCode.Unroutable);
	claim.close();
	origin.close();
});

test("announcing a job route again keeps it answering under one instance", async () => {
	const origin = new Producer();
	const path = Path.from("room/alice");
	const claim = origin.routePrefix(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(path);
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const job = pending.route({ hidden: true });
	job.broadcast.createTrack("data").writeFrame({ payload: new TextEncoder().encode("published") });
	job.announce();
	job.announce();
	await settle();
	expect(job.broadcast.closed.peek()).toBeUndefined();
	expect(request.active.peek()?.instance).toBe(job.instance);
	const reader = request.active.peek()?.track("data").subscribe().ordered();
	expect(await reader?.readString()).toBe("published");
	reader?.close();
	request.close();
	job.close();
	claim.close();
	origin.close();
});

for (const cleanup of ["close", "dispose", "broadcast", "announce"] as const) {
	test(`an old job route's ${cleanup} cannot refuse the next request for its path`, async () => {
		const origin = new Producer();
		const path = Path.from("room/alice");
		const claim = origin.routePrefix(Path.from("room"));
		const requests = claim.requested();
		const first = origin.request(path);
		const pending = (await requests.next()).value;
		if (!pending) throw new Error("expected a request");
		const old = pending.route({ hidden: true });
		first.close();
		await settle();
		const later = origin.request(path);
		const next = (await requests.next()).value;
		if (!next) throw new Error("expected another request");
		if (cleanup === "close") old.close();
		else if (cleanup === "dispose") old[Symbol.dispose]();
		else if (cleanup === "broadcast") old.broadcast.close();
		else expect(() => old.announce()).toThrow("unroutable");
		await settle();
		expect(later.closed.peek()).toBeUndefined();
		using job = next.route({ hidden: true });
		job.broadcast.createTrack("data").writeFrame({ payload: new TextEncoder().encode(cleanup) });
		job.announce();
		await settle();
		expect(later.active.peek()?.instance).toBe(job.instance);
		const reader = later.active.peek()?.track("data").subscribe().ordered();
		expect(await reader?.readString()).toBe(cleanup);
		reader?.close();
		old.close();
		later.close();
		claim.close();
		origin.close();
	});
}

test("a forwarded request cannot be routed by setting a publisher property", async () => {
	const origin = new Producer();
	const route = wireOf(origin).receive(Path.from("room"));
	const requests = route.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	Object.assign(pending, { publisher: origin });
	expect(() => pending.route()).toThrow("unroutable");
	pending.reject(new Error("refused"));
	request.close();
	route.close();
	origin.close();
});

test("an instance-pinned follower resumes when its instance returns after a gap", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const instance = Instance.random();
	const source = new BroadcastProducer();
	const keeper = source.consume();
	const serve = (instance: Instance.Valid, broadcast: BroadcastProducer) => {
		const route = origin.routePrefix(path, { instance: instance });
		void (async () => {
			for await (const request of route.requested()) request.serve(broadcast.consume());
		})();
		return route;
	};
	const first = serve(instance, source);
	const request = origin.follow(path, { instance });
	await settle();
	expect(request.active.peek()?.instance).toBe(instance);
	first.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeUndefined();
	const otherSource = new BroadcastProducer();
	const other = serve(Instance.random(), otherSource);
	await settle();
	expect(request.active.peek()).toBeUndefined();
	other.close();
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	const back = serve(instance, source);
	await settle();
	expect(request.active.peek()?.instance).toBe(instance);
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
	const publication = origin.route(path);
	await settle();
	expect(follower.active.peek()?.instance).toBe(publication.instance);
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
	const pinned = origin.request(Path.from("room/pinned"), { instance: Instance.random() });
	const follower = origin.follow(Path.from("room/followed"));
	// The initial set reports these before it lands, and nothing at all for the rest.
	const route = wire.receive(Path.from("room/found"));
	const other = wire.receive(Path.from("room/pinned"), { instance: Instance.random() });
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

// Unlike a Rust origin, which answers for its own table at once, a JS origin usually fronts a
// reconnecting connection: no session yet means nothing has synced.
test("a pending request waits for a session before concluding absence", async () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const request = origin.request(Path.from("room/missing"));
	await settle();
	expect(request.closed.peek()).toBeUndefined();

	let synced: (() => void) | undefined;
	const stop = wire.interests((prefix, wanted) => {
		if (wanted) synced = wire.snapshot(prefix);
	});
	const detach = wire.attach(true);
	await settle();
	expect(request.closed.peek()).toBeUndefined();
	synced?.();
	await settle();
	expect((request.closed.peek() as StreamError).code).toBe(StreamCode.Unroutable);

	request.close();
	stop();
	detach();
	origin.close();
});

test("a follower retains a selected handler's explicit unroutable refusal", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const route = origin.routePrefix(path);
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
	const first = origin.routePrefix(path);
	const requested = origin.request(path);
	const before = await first.requested().next();
	expect(before.value).toBeDefined();
	first.close();
	await settle();
	expect(requested.closed.peek()).toBeUndefined();
	const second = origin.routePrefix(path);
	const pending = (await second.requested().next()).value;
	if (!pending) throw new Error("replacement route was not requested");
	const source = new BroadcastProducer();
	const keeper = source.consume();
	pending.serve(source.consume());
	await settle();
	expect(requested.active.peek()?.instance).toBe(second.instance);
	const replacement = origin.route(path);
	await settle();
	expect(requested.active.peek()?.instance).toBe(second.instance);
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

test("a plain instance pin never takes an answer without that instance", () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(false);
	const path = Path.from("room/audio");
	const requested = origin.request(path, { instance: Instance.random() });
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

// Both instances stay live, and the cheaper one wins the path: a pin follows its own
// instance's routes, as Rust's lookup does, rather than filter the winner afterward.
test.each(["request", "follow"] as const)("a %s pinned to a losing instance resolves it", async (method) => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const pinned = Instance.random();
	const losing = origin.routePrefix(path, { instance: pinned, cost: 9n });
	const winning = origin.routePrefix(path, { instance: Instance.random(), cost: 1n });
	const asked = losing.requested();
	const requested = origin[method](path, { instance: pinned });
	const pending = (await asked.next()).value;
	if (!pending) throw new Error("the pinned instance's route was not asked");
	const source = new BroadcastProducer();
	pending.serve(source.consume());
	await settle();
	expect(requested.closed.peek()).toBeUndefined();
	expect(requested.active.peek()?.instance).toBe(pinned);

	// A second pin shares the answer, and the path's own requests still resolve the winner.
	const again = origin.request(path, { instance: pinned });
	expect(again.active.peek()?.instance).toBe(pinned);
	const plain = origin.request(path);
	const answer = (await winning.requested().next()).value;
	if (!answer) throw new Error("the winning route was not asked");
	const other = new BroadcastProducer();
	answer.serve(other.consume());
	await settle();
	expect(plain.active.peek()?.instance).toBe(winning.instance);

	requested.close();
	again.close();
	await settle();
	expect(plain.active.peek()?.instance).toBe(winning.instance);
	plain.close();
	source.close();
	other.close();
	losing.close();
	winning.close();
	origin.close();
});

test("a peer's pin is served by a local broadcast that loses the path", async () => {
	const origin = new Producer();
	const wire = wireOf(origin.consume());
	const path = Path.from("room/audio");
	const pinned = Instance.random();
	const publication = origin.route(path, { instance: pinned, cost: 9n });
	const winning = origin.routePrefix(path, { instance: Instance.random(), cost: 1n });
	expect(wire.local(path)).toBeUndefined();
	expect(wire.local(path, pinned)).toBeDefined();
	expect(await wire.demand(path, pinned)).toBeDefined();
	winning.close();
	publication.broadcast.close();
	origin.close();
});

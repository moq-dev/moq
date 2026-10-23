import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast";
import * as Epoch from "./epoch";
import { StreamCode, StreamError } from "./error";
import { Route } from "./hop";
import { Producer } from "./origin";
import * as Path from "./path";
import { wireOf } from "./wire";

const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

test("an announced request follows restart, end and a later start", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const first = origin.createBroadcast(path);
	first.announce();
	const request = origin.request(path, { announced: true });
	expect(request.active.peek()?.closed).toBe(first.closed);
	const second = origin.createBroadcast(path);
	second.announce();
	await settle();
	expect(request.active.peek()?.closed).toBe(second.closed);
	second.close();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeUndefined();
	const third = origin.createBroadcast(path);
	third.announce();
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
	const first = origin.createBroadcast(path);
	first.announce({ epoch });
	const request = origin.request(path, { announced: true, epoch });
	const second = origin.createBroadcast(path);
	second.announce({ epoch: Epoch.mint() });
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
	const broadcast = origin.createBroadcast(path);
	const before = Epoch.mint();
	broadcast.announce({ epoch: before });
	const request = origin.request(path, { announced: true });
	const old = request.active.peek();
	const after = Epoch.mint();
	broadcast.announce({ epoch: after });
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
	const broadcast = origin.createBroadcast(path);
	broadcast.announce();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	expect(request.closed.peek()).toBeNull();
	broadcast.close();
	origin.close();
});

test.each([false, true])(
	"requests refuse unwatchable paths and scope violations before registering (announced: %s)",
	(announced) => {
		const origin = new Producer();
		expect(() => origin.request(Path.from("camera*main"), { announced })).toThrow();
		const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.subtree(Path.from("pool/job/cam"))]));
		expect(() => scoped.request(Path.from("pool/job"), { announced })).toThrow();
		expect(wireOf(origin).requests.peek()?.size).toBe(0);
		origin.close();
	},
);

test("unrelated routes never notify announced request followers", async () => {
	const origin = new Producer();
	const requests = Array.from({ length: 32 }, (_, i) => origin.request(Path.from(`room/${i}`), { announced: true }));
	await settle();
	let changes = 0;
	const stops = requests.map((request) => request.active.subscribe(() => changes++));
	const other = origin.createBroadcast(Path.from("unrelated"));
	other.announce();
	await settle();
	other.announce({ cost: 5n });
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

test("only a plain request can use a session without discovery", async () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(false);
	const path = Path.from("blind");
	const followed = origin.request(path, { announced: true });
	const slot = wire.requests.peek()?.get(path);
	if (!slot) throw new Error("request did not register");
	expect(wire.blind(slot)).toBe(false);
	const requested = origin.request(path);
	expect(wire.blind(slot)).toBe(true);
	requested.close();
	await settle();
	expect(wire.blind(slot)).toBe(false);
	followed.close();
	detach();
	origin.close();
});

test("announced requests follow the serving prefix, ignoring descendants and re-prices", async () => {
	const origin = new Producer();
	const source = new BroadcastProducer();
	const keeper = source.consume();
	const prefix = origin.dynamicRoute(Path.from("pool"));
	const serving = (async () => {
		for await (const request of prefix.requested()) request.accept(source.consume());
	})();
	const request = origin.request(Path.from("pool/job"), { announced: true });
	await settle();
	expect(request.active.peek()?.closed).toBe(source.closed);
	const below = origin.createBroadcast(Path.from("pool/job/thumb"));
	below.announce();
	await settle();
	expect(request.active.peek()?.closed).toBe(source.closed);
	const exact = origin.createBroadcast(Path.from("pool/job"));
	exact.announce();
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
	const exact = origin.createBroadcast(Path.from("room/alice"));
	exact.announce({ cost: 5n });
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
			for await (const request of route.requested()) request.accept(source.consume());
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
		for await (const request of route.requested()) request.accept(source.consume());
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
	test(`a plain request owns discovery through both local and received routes (received: ${received})`, async () => {
		const origin = new Producer();
		const path = Path.from("room");
		const events: string[] = [];
		const stop = wireOf(origin).interests((prefix, wanted) => events.push(`${prefix} ${wanted}`));
		const source = new BroadcastProducer();
		const route = received ? wireOf(origin).receive(path, Route.default) : origin.dynamicRoute(path);
		void (async () => {
			for await (const request of route.requested()) request.accept(source.consume());
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
	published.close();
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
	expect(() => prepared.broadcast.announce({ epoch: Epoch.mint() })).toThrow("not attached to an origin");
	expect(prepared.broadcast.route).toBeUndefined();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	const published = prepared.publish();
	expect(published.route?.epoch).toBe(epoch);
	await settle();
	expect(request.active.peek()?.epoch).toBe(epoch);
	request.close();
	published.close();
	claim.close();
	origin.close();
});

test("an application claim refuses an arbitrary broadcast", async () => {
	const origin = new Producer();
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	expect(() => pending.accept(new BroadcastProducer())).toThrow("prepared and published");
	request.close();
	claim.close();
	origin.close();
});

test("rejecting a prepared answer refuses the request and closes its broadcast", async () => {
	const origin = new Producer();
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const prepared = pending.prepare(Epoch.mint());
	prepared.reject(new Error("no output"));
	await settle();
	expect(request.closed.peek()).toBeInstanceOf(Error);
	expect(prepared.broadcast.closed.peek()).toBeDefined();
	claim.close();
	origin.close();
});

test("a prepared answer that fails to publish refuses the request", async () => {
	const origin = new Producer();
	const claim = origin.dynamic(Path.from("room"));
	const requests = claim.requested();
	const request = origin.request(Path.from("room/alice"));
	const pending = (await requests.next()).value;
	if (!pending) throw new Error("expected a request");
	const prepared = pending.prepare(Epoch.mint());
	prepared.broadcast.close();
	expect(() => prepared.publish()).toThrow("broadcast is closed");
	await settle();
	expect(request.closed.peek()).toBeInstanceOf(Error);
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

test("an epoch-pinned follower resumes when its instance returns after a gap", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const epoch = Epoch.mint();
	const source = new BroadcastProducer();
	const keeper = source.consume();
	const serve = (instance: Epoch.Valid, broadcast: BroadcastProducer) => {
		const route = origin.dynamicRoute(path, { epoch: instance });
		void (async () => {
			for await (const request of route.requested()) request.accept(broadcast.consume());
		})();
		return route;
	};
	const first = serve(epoch, source);
	const request = origin.request(path, { announced: true, epoch });
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

test("a request waits for its scoped discovery boundary before reporting absence", async () => {
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
	expect(request.unroutable.peek()).toBe(false);
	const unrelated = wire.snapshot(Path.from("other"));
	synced?.();
	expect(request.unroutable.peek()).toBe(true);
	unrelated();
	request.close();
	expect(interests).toEqual(["room/audio true", "room/audio false"]);
	stop();
	detach();
	origin.close();
});

test("an announced request retains a selected handler's explicit unroutable refusal", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const route = origin.dynamicRoute(path);
	const pending = route.requested();
	const requested = origin.request(path, { announced: true });
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

test("a pending plain request retries a route gap and never retargets after resolving", async () => {
	const origin = new Producer();
	const path = Path.from("room/audio");
	const first = origin.dynamicRoute(path);
	const requested = origin.request(path);
	const before = await first.requested().next();
	expect(before.value).toBeDefined();
	first.close();
	await settle();
	expect(requested.closed.peek()).toBeUndefined();
	const second = origin.dynamicRoute(path);
	const pending = (await second.requested().next()).value;
	if (!pending) throw new Error("replacement route was not requested");
	const source = new BroadcastProducer();
	const keeper = source.consume();
	pending.accept(source.consume());
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
	replacement.close();
	keeper.close();
	source.close();
	origin.close();
});

test("a plain epoch pin waits for an answer and refuses one without that epoch", () => {
	const origin = new Producer();
	const wire = wireOf(origin);
	const detach = wire.attach(false);
	const path = Path.from("room/audio");
	const requested = origin.request(path, { epoch: Epoch.mint() });
	expect(requested.unroutable.peek()).toBe(false);
	const source = new BroadcastProducer();
	const withdraw = wire.answer(path, source.consume());
	expect(requested.active.peek()).toBeUndefined();
	expect(requested.unroutable.peek()).toBe(true);
	requested.close();
	withdraw?.();
	source.close();
	detach();
	origin.close();
});

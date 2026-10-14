import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast";
import * as Epoch from "./epoch";
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
	const prefix = origin.dynamic(Path.from("pool"));
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
	prefix.update({ cost: 5n });
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
	const prefix = scoped.dynamic(Path.from("room/alice"));
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
		const route = origin.dynamic(path);
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
	const route = origin.dynamic(path);
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
	const route = origin.dynamic(path);
	const requested = route.requested();
	const request = origin.request(path, { announced: true });
	const pending = (await requested.next()).value;
	if (!pending) throw new Error("expected a request");

	// Both land in one turn: the watch notification must not retry the refused request.
	route.update({ cost: 5n });
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
		const route = received ? wireOf(origin).receive(path, Route.default) : origin.dynamic(path);
		void (async () => {
			for await (const request of route.requested()) request.accept(source.consume());
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

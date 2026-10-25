import { expect, spyOn, test } from "bun:test";
import { Producer as Broadcast } from "./broadcast.ts";
import * as Epoch from "./epoch.ts";
import { randomHop } from "./hop.ts";
import { NativeSession } from "./ietf/adapter.ts";
import { Publisher as IetfPublisher } from "./ietf/publisher.ts";
import { Subscribe as IetfSubscribe } from "./ietf/subscribe.ts";
import { Version as IetfVersion } from "./ietf/version.ts";
import { hooks } from "./internal.ts";
import { Fetch } from "./lite/fetch.ts";
import { Publisher as LitePublisher } from "./lite/publisher.ts";
import { Subscribe as LiteSubscribe } from "./lite/subscribe.ts";
import { Track } from "./lite/track.ts";
import { Version as LiteVersion } from "./lite/version.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";
import { readsFrom } from "./resume.test-util";
import { Stream } from "./stream.ts";
import { wireOf } from "./wire.ts";

// Let stream errors and the origin's deferred slot release finish this turn, without sleeping.
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));
const path = Path.from("room/video");

test.each(["lite", "ietf", "fetch", "track"] as const)(
	"%s cancellation withdraws unanswered origin requests",
	async (kind) => {
		const pair = createMockTransportPair();
		const origin = new Producer();
		const dynamic = origin.requestsRoute(Path.from("room"));
		const publisher =
			kind === "ietf"
				? new IetfPublisher({
						quic: pair.server,
						session: new NativeSession(pair.server, IetfVersion.DRAFT_22, false),
						publish: origin.consume(),
						requiresSolicitation: false,
					})
				: new LitePublisher(pair.server, LiteVersion.DRAFT_06, randomHop(), origin.consume());
		const tasks: Promise<void>[] = [];
		try {
			for (let i = 0; i < 32; i++) {
				const name = Path.from(`room/${i}`);
				let input!: ReadableStreamDefaultController<Uint8Array>;
				const stream = new Stream({
					version: kind === "ietf" ? IetfVersion.DRAFT_22 : LiteVersion.DRAFT_06,
					readable: new ReadableStream({
						start(c) {
							input = c;
						},
					}),
					writable: new WritableStream(),
				});
				const task =
					publisher instanceof IetfPublisher
						? publisher.runSubscribe(
								new IetfSubscribe({
									requestId: BigInt(i * 2),
									trackNamespace: name,
									trackName: "video",
									subscriberPriority: 0,
								}),
								stream,
							)
						: kind === "fetch"
							? publisher.runFetch(
									new Fetch({ broadcast: name, track: "video", group: 0, priority: 0 }),
									stream,
								)
							: kind === "track"
								? publisher.runTrackInfo(new Track(name, "video"), stream)
								: publisher.runSubscribe(
										new LiteSubscribe({
											id: BigInt(i),
											broadcast: name,
											track: "video",
											priority: 0,
										}),
										stream,
									);
				let ended = false;
				tasks.push(
					task.finally(() => {
						ended = true;
					}),
				);
				input.error(new Error("request cancelled"));
				await settle();
				expect(ended).toBe(true);
			}
			// A new path is the only queued work: none of the cancelled paths reaches the handler.
			const local = origin.request(path);
			const requests = dynamic.requested();
			expect((await requests.next()).value?.path).toBe(path);
			local.close();
			await requests.return?.();
		} finally {
			origin.close();
			await Promise.allSettled(tasks);
			publisher.close();
			pair.client.close();
			pair.server.close();
		}
	},
);

test("last cancellation finishes the internal wait and discards a late consumer", async () => {
	const origin = new Producer();
	const dynamic = origin.requestsRoute(Path.from("room"));
	const requests = dynamic.requested();
	const hold = new AbortController();
	const wait = wireOf(origin.consume()).demand(path, undefined, hold.signal);
	let ended = false;
	const result = wait.catch(() => {
		ended = true;
	});
	const old = (await requests.next()).value;
	if (!old) throw new Error("expected a pending request");
	try {
		hold.abort();
		await settle();
		expect(ended).toBe(true);
		const next = origin.request(path);
		const fresh = (await requests.next()).value;
		if (!fresh) throw new Error("expected a pending request");
		expect(fresh).not.toBe(old);
		const source = new Broadcast();
		const keeper = source.consume();
		const late = source.consume();
		const close = spyOn(late, "close");
		hooks.acceptRequest(old, late);
		expect(close).toHaveBeenCalledTimes(1);
		close.mockRestore();
		expect(source.closed.peek()).toBeUndefined();
		hooks.acceptRequest(fresh, source);
		await settle();
		expect(next.active.peek()?.closed).toBe(source.closed);
		next.close();
		keeper.close();
		source.close();
	} finally {
		origin.close();
		await result;
		await requests.return?.();
	}
});

test.each([true, false])(
	"shared local/remote demand is withdrawn whichever leaves last (local first: %s)",
	async (localFirst) => {
		const origin = new Producer();
		const dynamic = origin.requestsRoute(Path.from("room"));
		const requests = dynamic.requested();
		const hold = new AbortController();
		const wait = wireOf(origin.consume())
			.demand(path, undefined, hold.signal)
			.catch(() => {});
		const local = origin.request(path);
		const old = (await requests.next()).value;
		if (!old) throw new Error("expected a pending request");
		try {
			if (localFirst) {
				local.close();
				await settle();
				hold.abort();
			} else {
				hold.abort();
				await settle();
				local.close();
			}
			await settle();
			const late = new Broadcast();
			const front = late.consume();
			hooks.acceptRequest(old, front);
			expect(late.closed.peek()).toBeNull();
			const next = origin.request(path);
			const fresh = (await requests.next()).value;
			if (!fresh) throw new Error("expected a pending request");
			expect(fresh).not.toBe(old);
			next.close();
		} finally {
			origin.close();
			await wait;
			await requests.return?.();
		}
	},
);

test.each([
	["peer", 1],
	["peer", 16],
	["local", 1],
	["local", 16],
] as const)("cancelling one waiter preserves a second %s requester across %i routes", async (other, routes) => {
	const origin = new Producer();
	const prefix = Path.from("room");
	const dynamics = Array.from({ length: routes }, (_, i) => origin.requestsRoute(prefix, { cost: BigInt(i) }));
	const hold = new AbortController();
	const wire = wireOf(origin.consume());
	const first = wire.demand(path, undefined, hold.signal).catch(() => undefined);
	// With competing routes, re-price the cancelled waiter's route so the survivor pends on another.
	const cancelled = dynamics[0];
	if (routes > 1) cancelled.updateRoute({ ...cancelled.route, cost: BigInt(routes) });
	const requests = dynamics[routes > 1 ? 1 : 0].requested();
	const second = other === "peer" ? wire.demand(path) : undefined;
	const local = other === "local" ? origin.request(path) : undefined;
	const request = (await requests.next()).value;
	if (!request) throw new Error("expected a pending request");
	const source = new Broadcast();
	try {
		hold.abort();
		await first;
		hooks.acceptRequest(request, source);
		await settle();
		expect(other === "peer" ? (await second)?.closed : local?.active.peek()?.closed).toBe(source.closed);
	} finally {
		local?.close();
		origin.close();
		source.close();
		await requests.return?.();
	}
});

test("an already cancelled signal never enqueues or subscribes to abort", async () => {
	const origin = new Producer();
	const dynamic = origin.requestsRoute(Path.from("room"));
	const requests = dynamic.requested();
	const hold = new AbortController();
	hold.abort();
	const listen = spyOn(hold.signal, "addEventListener");
	try {
		await expect(wireOf(origin.consume()).demand(path, undefined, hold.signal)).rejects.toThrow();
		expect(listen).not.toHaveBeenCalled();
		const next = origin.request(Path.from("room/live"));
		expect((await requests.next()).value?.path).toBe(Path.from("room/live"));
		next.close();
	} finally {
		listen.mockRestore();
		origin.close();
		await requests.return?.();
	}
});

test.each(["accept", "reject", "cancel"] as const)(
	"%s releases the abort listener without closing a shared producer",
	async (action) => {
		const origin = new Producer();
		const dynamic = origin.requestsRoute(Path.from("room"));
		const requests = dynamic.requested();
		const source = new Broadcast();
		const keeper = source.consume();
		const hold = new AbortController();
		const remove = spyOn(hold.signal, "removeEventListener");
		const wait = wireOf(origin.consume())
			.demand(path, undefined, hold.signal)
			.catch(() => undefined);
		const request = (await requests.next()).value;
		if (!request) throw new Error("expected a pending request");
		try {
			if (action === "accept") hooks.acceptRequest(request, source);
			if (action === "reject") request.reject(new Error("denied"));
			if (action === "cancel") hold.abort();
			const result = await wait;
			expect(remove).toHaveBeenCalledTimes(1);
			hold.abort();
			if (action === "accept") {
				expect(result?.closed).toBe(source.closed);
				hooks.acceptRequest(request, source);
				request.reject(new Error("late"));
			} else hooks.acceptRequest(request, source);
			expect(source.closed.peek()).toBeUndefined();
		} finally {
			remove.mockRestore();
			origin.close();
			keeper.close();
			source.close();
			await requests.return?.();
		}
	},
);

test("a pending local request changing routes withdraws the old route's work", async () => {
	const origin = new Producer();
	const first = origin.requestsRoute(Path.from("room"), { cost: 5n });
	const a = first.requested();
	const local = origin.request(path);
	const old = (await a.next()).value;
	if (!old) throw new Error("expected a pending request");
	const second = origin.requestsRoute(Path.from("room"), { cost: 1n });
	const b = second.requested();
	const fresh = (await b.next()).value;
	if (!fresh) throw new Error("expected a pending request");
	const source = new Broadcast();
	const keeper = source.consume();
	const late = source.consume();
	const close = spyOn(late, "close");
	try {
		hooks.acceptRequest(old, late);
		expect(close).toHaveBeenCalledTimes(1);
		hooks.acceptRequest(fresh, source);
		await settle();
		expect(local.active.peek()?.closed).toBe(source.closed);
	} finally {
		close.mockRestore();
		local.close();
		origin.close();
		keeper.close();
		source.close();
		await a.return?.();
		await b.return?.();
	}
});

test("cancelling an old epoch waiter does not decrement a new epoch's demand", async () => {
	const origin = new Producer();
	const epoch = Epoch.mint();
	const dynamic = origin.requestsRoute(Path.from("room"), { epoch });
	const requests = dynamic.requested();
	const wire = wireOf(origin.consume());
	const hold = new AbortController();
	const before = wire.demand(path, epoch, hold.signal).catch(() => undefined);
	const old = (await requests.next()).value;
	if (!old) throw new Error("expected old request");
	const nextEpoch = Epoch.mint();
	dynamic.updateRoute({ epoch: nextEpoch });
	const after = wire.demand(path, nextEpoch);
	hold.abort();
	await before;
	const fresh = (await requests.next()).value;
	if (!fresh) throw new Error("expected new request");
	const source = new Broadcast();
	try {
		old.reject(new Error("obsolete"));
		hooks.acceptRequest(fresh, source);
		expect((await after)?.closed).toBe(source.closed);
	} finally {
		origin.close();
		source.close();
		await requests.return?.();
	}
});

test.each(["answer", "release"] as const)(
	"a detached same-epoch failover survives peer cancellation until %s",
	async (action) => {
		const origin = new Producer();
		const epoch = Epoch.mint();
		const prefix = Path.from("room");
		const a = origin.requestsRoute(prefix, { epoch, cost: 1n });
		const b = origin.requestsRoute(prefix, { epoch, cost: 5n });
		const ar = a.requested();
		const br = b.requested();
		const held = origin.request(path);
		const source = new Broadcast();
		hooks.acceptRequest((await ar.next()).value, source);
		await settle();
		// Another instance wins new requests. The old handle stays on its own epoch.
		const c = origin.requestsRoute(prefix, { epoch: Epoch.mint(), cost: 0n });
		a.close();
		await settle();
		const pending = (await br.next()).value;
		if (!pending) throw new Error("expected failover request");
		// Remove the winning instance so a wire waiter also chooses b.
		c.close();
		const hold = new AbortController();
		const wait = wireOf(origin.consume())
			.demand(path, epoch, hold.signal)
			.catch(() => {});
		hold.abort();
		await wait;
		// The detached handle still owns the request until it releases or receives an answer.
		if (action === "release") {
			held.close();
			await settle();
		}
		const late = new Broadcast();
		const front = late.consume();
		const close = spyOn(front, "close");
		try {
			hooks.acceptRequest(pending, front);
			await settle();
			if (action === "release") expect(close).toHaveBeenCalledTimes(1);
			else {
				expect(close).not.toHaveBeenCalled();
				expect(readsFrom(held.active.peek(), late)).toBe(true);
			}
		} finally {
			close.mockRestore();
			held.close();
			origin.close();
			source.close();
			late.close();
			await ar.return?.();
			await br.return?.();
		}
	},
);

test.each([0, 1])(
	"failover cleanup excludes released slot %s without withdrawing another instance",
	async (released) => {
		const origin = new Producer();
		const prefix = Path.from("room");
		const entries = [];
		const sources: Broadcast[] = [];
		for (let i = 0; i < 2; i++) {
			const epoch = Epoch.parse(`00000000-0000-7000-8000-00000000000${i + 1}`);
			const primary = origin.requestsRoute(prefix, { epoch, cost: 0n });
			const standby = origin.requestsRoute(prefix, { epoch, cost: 1n });
			const ar = primary.requested();
			const br = standby.requested();
			const held = origin.request(path);
			const source = new Broadcast();
			sources.push(source);
			hooks.acceptRequest((await ar.next()).value, source);
			await settle();
			entries.push({ primary, ar, br, held });
		}
		origin.requestsRoute(prefix, { epoch: Epoch.parse("00000000-0000-7000-8000-000000000003") });
		const pending = [];
		for (const entry of entries) {
			entry.primary.close();
			const request = (await entry.br.next()).value;
			if (!request) throw new Error("expected same-epoch failover");
			pending.push(request);
		}
		const hold = new AbortController();
		const wait = wireOf(origin.consume())
			.demand(path, undefined, hold.signal)
			.catch(() => undefined);
		const late = new Broadcast();
		const front = late.consume();
		const close = spyOn(front, "close");
		const replacement = new Broadcast();
		try {
			// The slot is still detached until its microtask runs, but no longer owns demand.
			entries[released].held.close();
			hold.abort();
			hooks.acceptRequest(pending[released], front);
			expect(close).toHaveBeenCalledTimes(1);
			hooks.acceptRequest(pending[1 - released], replacement);
			await wait;
			expect(readsFrom(entries[1 - released].held.active.peek(), replacement)).toBe(true);
		} finally {
			close.mockRestore();
			for (const entry of entries) entry.held.close();
			origin.close();
			for (const source of [...sources, late, replacement]) source.close();
			for (const entry of entries) {
				await entry.ar.return?.();
				await entry.br.return?.();
			}
		}
	},
);

test("a normal FETCH request FIN keeps unanswered origin demand alive", async () => {
	const pair = createMockTransportPair();
	const origin = new Producer();
	const dynamic = origin.requestsRoute(Path.from("room"));
	const requests = dynamic.requested();
	const publisher = new LitePublisher(pair.server, LiteVersion.DRAFT_06, randomHop(), origin.consume());
	let input!: ReadableStreamDefaultController<Uint8Array>;
	const stream = new Stream({
		version: LiteVersion.DRAFT_06,
		readable: new ReadableStream({
			start(c) {
				input = c;
			},
		}),
		writable: new WritableStream(),
	});
	let ended = false;
	const task = publisher
		.runFetch(new Fetch({ broadcast: path, track: "video", group: 0, priority: 0 }), stream)
		.finally(() => {
			ended = true;
		});
	const request = (await requests.next()).value;
	if (!request) throw new Error("expected request");
	try {
		input.close();
		await settle();
		expect(ended).toBe(false);
		request.reject(new Error("handler refused"));
		await task;
		expect(ended).toBe(true);
	} finally {
		origin.close();
		publisher.close();
		pair.client.close();
		pair.server.close();
		await requests.return?.();
	}
});

test.each(["accept", "reject"] as const)(
	"cancellation racing %s releases only the cancelled waiter",
	async (answer) => {
		const origin = new Producer();
		const dynamic = origin.requestsRoute(Path.from("room"));
		const requests = dynamic.requested();
		const wire = wireOf(origin.consume());
		const hold = new AbortController();
		const cancelled = new Error("cancelled");
		const first = wire.demand(path, undefined, hold.signal).catch((error: unknown) => error);
		const second = wire.demand(path).catch((error: unknown) => error);
		const request = (await requests.next()).value;
		if (!request) throw new Error("expected request");
		const source = new Broadcast();
		const denied = new Error("denied");
		try {
			hold.abort(cancelled);
			if (answer === "accept") hooks.acceptRequest(request, source);
			else request.reject(denied);
			expect(await first).toBe(cancelled);
			const result = await second;
			if (answer === "accept") expect((result as { closed: unknown }).closed).toBe(source.closed);
			else expect(result).toBe(denied);
		} finally {
			origin.close();
			source.close();
			await requests.return?.();
		}
	},
);

test.each([true, false])("last waiter cancellation and accept in one turn (cancel first: %s)", async (cancelFirst) => {
	const origin = new Producer();
	const dynamic = origin.requestsRoute(Path.from("room"));
	const requests = dynamic.requested();
	const wire = wireOf(origin.consume());
	const hold = new AbortController();
	const cancelled = new Error("cancelled");
	const wait = wire.demand(path, undefined, hold.signal).catch((error: unknown) => error);
	const request = (await requests.next()).value;
	if (!request) throw new Error("expected request");
	const source = new Broadcast();
	const keeper = source.consume();
	const front = source.consume();
	const close = spyOn(front, "close");
	try {
		if (cancelFirst) hold.abort(cancelled);
		hooks.acceptRequest(request, front);
		if (!cancelFirst) hold.abort(cancelled);
		expect(await wait).toBe(cancelled);
		expect(close).toHaveBeenCalledTimes(cancelFirst ? 1 : 0);
		expect(source.closed.peek()).toBeUndefined();
		const next = wire.demand(path);
		if (cancelFirst) {
			const fresh = (await requests.next()).value;
			expect(fresh).not.toBe(request);
			if (!fresh) throw new Error("expected fresh request");
			hooks.acceptRequest(fresh, source);
		}
		const answer = await next;
		expect(answer?.closed).toBe(source.closed);
		if (cancelFirst) expect(answer).not.toBe(front);
		else expect(answer).toBe(front);
	} finally {
		close.mockRestore();
		origin.close();
		keeper.close();
		source.close();
		await requests.return?.();
	}
});

test.each([true, false])("local demand survives same-turn cancellation only when rejoined: %s", async (rejoin) => {
	const origin = new Producer();
	const dynamic = origin.requestsRoute(Path.from("room"));
	const requests = dynamic.requested();
	const local = origin.request(path);
	const hold = new AbortController();
	const wait = wireOf(origin.consume())
		.demand(path, undefined, hold.signal)
		.catch(() => {});
	const request = (await requests.next()).value;
	if (!request) throw new Error("expected pending request");
	const source = new Broadcast();
	const keeper = source.consume();
	const front = source.consume();
	const close = spyOn(front, "close");
	local.close();
	hold.abort();
	const replacement = rejoin ? origin.request(path) : undefined;
	try {
		await settle();
		hooks.acceptRequest(request, front);
		expect(close).toHaveBeenCalledTimes(rejoin ? 0 : 1);
		if (replacement) {
			await settle();
			expect(replacement.active.peek()?.closed).toBe(source.closed);
		} else {
			const fresh = origin.request(path);
			const next = (await requests.next()).value;
			expect(next).not.toBe(request);
			if (!next) throw new Error("expected fresh request");
			hooks.acceptRequest(next, source);
			await settle();
			expect(fresh.active.peek()?.closed).toBe(source.closed);
			fresh.close();
		}
	} finally {
		close.mockRestore();
		replacement?.close();
		origin.close();
		keeper.close();
		source.close();
		await wait;
		await requests.return?.();
	}
});

test("queued cancellation preserves order, replacement identity, and empty-queue wakeup", async () => {
	const origin = new Producer();
	const dynamic = origin.requestsRoute(Path.from("room"));
	const requests = dynamic.requested();
	const wire = wireOf(origin.consume());
	const holds = Array.from({ length: 4 }, () => new AbortController());
	const waits = holds.map((hold, i) => wire.demand(Path.from(`room/${i}`), undefined, hold.signal).catch(() => {}));
	try {
		expect((await requests.next()).value?.path).toBe(Path.from("room/0"));
		// Remove queued entries while the iterator is suspended, then reuse one path.
		holds[1].abort();
		holds[3].abort();
		waits.push(wire.demand(Path.from("room/1")).catch(() => {}));
		expect((await requests.next()).value?.path).toBe(Path.from("room/2"));
		expect((await requests.next()).value?.path).toBe(Path.from("room/1"));
		const waiting = requests.next();
		waits.push(wire.demand(Path.from("room/new")).catch(() => {}));
		expect((await waiting).value?.path).toBe(Path.from("room/new"));
	} finally {
		origin.close();
		await Promise.all(waits);
		await requests.return?.();
	}
});

test("reset clears queued work while a handler is suspended and admits the new epoch", async () => {
	const origin = new Producer();
	const epoch = Epoch.mint();
	const dynamic = origin.requestsRoute(Path.from("room"), { epoch });
	const requests = dynamic.requested();
	const wire = wireOf(origin.consume());
	const waits = [path, Path.from("room/obsolete")].map((name) => wire.demand(name, epoch).catch(() => {}));
	try {
		const old = (await requests.next()).value;
		expect(old?.path).toBe(path);
		const nextEpoch = Epoch.mint();
		dynamic.updateRoute({ epoch: nextEpoch });
		waits.push(wire.demand(path, nextEpoch).catch(() => {}));
		const fresh = (await requests.next()).value;
		expect(fresh?.path).toBe(path);
		expect(fresh).not.toBe(old);
		old?.reject(new Error("obsolete"));
		fresh?.reject(new Error("new epoch refused"));
	} finally {
		origin.close();
		await Promise.all(waits);
		await requests.return?.();
	}
});

test("concurrent handlers consume queued requests once across empty batches", async () => {
	const origin = new Producer();
	const dynamic = origin.requestsRoute(Path.from("room"));
	const handlers = [dynamic.requested(), dynamic.requested()];
	const wire = wireOf(origin.consume());
	const waits: Promise<unknown>[] = [];
	try {
		for (let batch = 0; batch < 2; batch++) {
			const waiting = handlers.map((handler) => handler.next());
			const paths = [0, 1].map((i) => Path.from(`room/${batch}/${i}`));
			for (const name of paths) waits.push(wire.demand(name).catch(() => {}));
			const requests = await Promise.all(waiting);
			expect(requests.map((request) => request.value?.path).sort()).toEqual(paths);
			for (const request of requests) request.value?.reject(new Error("refused"));
		}
	} finally {
		origin.close();
		await Promise.all(waits);
		for (const handler of handlers) await handler.return?.();
	}
});

import { afterEach, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { accept as acceptConnection } from "./connection/accept.ts";
import { connect } from "./connection/connect.ts";
import { NotFound, StreamCode, StreamError } from "./error.ts";
import * as Ietf from "./ietf/index.ts";
import { Origin, Path, Socket } from "./index.ts";
import { hooks } from "./internal.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import type * as Track from "./track.ts";
import { wireOf } from "./wire.ts";

const origins = new Set<Origin.Producer>();
const clocks: { close(): void }[] = [];
afterEach(() => {
	for (const origin of origins) origin.close();
	origins.clear();
	for (const clock of clocks.reverse()) clock.close();
	clocks.length = 0;
});
function time() {
	const clock = new Manual();
	clocks.push(clock.install());
	return clock;
}
const bytes = (value: number) => ({ payload: new Uint8Array([value]) });
function setup() {
	const origin = new Origin.Producer();
	origins.add(origin);
	const a = new Socket.Endpoint(origin, Path.from("room"), "alice");
	const b = new Socket.Endpoint(origin, Path.from("room"), "bob");
	return { origin, a, b };
}
async function track() {
	const state = setup();
	const incoming = state.b.acceptTrack();
	const dialing = state.a.dialTrack("bob");
	const remote = await (await incoming).accept();
	return { ...state, local: await dialing, remote };
}
type Role = ".server" | ".client";
function announced(origin: Origin.Producer, role: Role, id: string): BroadcastConsumer | undefined {
	return origin.request(Path.from("room", role, id)).active.peek();
}
async function broadcast(origin: Origin.Producer, role: Role, id: string): Promise<BroadcastConsumer> {
	const active = announced(origin, role, id);
	if (!active) throw new Error("endpoint not routed locally");
	return active;
}
async function raw(origin: Origin.Producer, role: Role, id: string, name: string): Promise<Track.Subscriber> {
	return (await broadcast(origin, role, id)).track(name).subscribe({ order: "ascending", delay: { max: null } });
}

test("Socket namespace exposes endpoint and stream", () => {
	expect(Socket.Endpoint).toBeFunction();
	expect(Socket.TrackStream).toBeFunction();
});

test("isEndpoint tells an endpoint's broadcast from media", () => {
	for (const role of [".server", ".client"]) {
		expect(Socket.isEndpoint(Path.from("room", role, "alice"))).toBeTrue();
	}
	for (const path of ["room/alice", ".server", "room/.server", "room/.server/alice/cam", "room/server/alice"]) {
		expect(Socket.isEndpoint(Path.from(path))).toBeFalse();
	}
});

test("an endpoint closes with its origin before it dials or accepts", async () => {
	const origin = new Origin.Producer();
	const endpoint = new Socket.Endpoint(origin, Path.from("room"), "alice");
	origin.close();
	expect(await endpoint.closed).toBeNull();
	expect(new Socket.Endpoint(origin, Path.from("room"), "bob").closed.peek()).toBeNull();
});

test("a peer route replaced between metadata and opening cannot inherit a socket session", async () => {
	const clock = time();
	const { origin, a } = setup();
	const peer = new BroadcastProducer();
	const keep = peer.consume();
	const routes: { close(): void }[] = [];
	const tasks: Promise<void>[] = [];
	const advertise = (front: BroadcastConsumer) => {
		const route = wireOf(origin).receive(Path.from("room/.server/remote"));
		routes.push(route);
		tasks.push(
			(async () => {
				try {
					for await (const request of route.requested()) request.serve(front.clone());
				} catch {
					/* Closing the route ends its request loop. */
				}
			})(),
		);
		return route;
	};
	const first = advertise(keep);
	const requested = wireOf(peer).requested();
	let outcome: unknown;
	const dialing = a.dialTrack("remote").then(
		(value) => {
			outcome = value;
		},
		(error) => {
			outcome = error;
		},
	);
	let returning: Track.Subscriber | undefined;
	const replacement = new BroadcastProducer();
	const replacementKeep = replacement.consume();
	try {
		const request = await requested;
		if (!request) throw new Error("missing dial");
		returning = await raw(origin, ".client", "alice", request.name);
		await returning.info();
		request.accept({ maxAge: null });
		// Both route changes happen before the info promise's continuation opens the session.
		first.close();
		advertise(replacementKeep);
		await clock.advance(Duration.ZERO);
		expect(outcome).toBeInstanceOf(Error);
	} finally {
		a.close();
		for (const route of routes) route.close();
		returning?.close();
		keep.close();
		replacementKeep.close();
		peer.close();
		replacement.close();
		await Promise.all(tasks);
		await dialing;
	}
});

test("track halves start reliable and ascending, preserve old messages and accept independent info", async () => {
	const { local, remote } = await track();
	expect(local.peer).toBe("bob");
	expect(remote.peer).toBe("alice");
	expect(local.session).toBe(remote.session);
	expect(local.session).toMatch(/^[0-9a-f]{32}$/);
	expect((await local.producer.info()).maxAge).toBeNull();
	expect(remote.consumer.subscription.peek()?.order).toBe("ascending");
	expect(remote.consumer.subscription.peek()?.delay?.max).toEqual(Duration.millis(Number.MAX_SAFE_INTEGER));
	for (let i = 0; i < 3; i++) local.producer.writeFrame(bytes(i));
	local.producer.finish();
	const reader = remote.consumer.ordered();
	for (let i = 0; i < 3; i++) {
		const frame = await reader.readFrame();
		expect(frame?.group).toBe(BigInt(i));
		expect(frame?.payload).toEqual(bytes(i).payload);
	}
	expect(await reader.readFrame()).toBeUndefined();
	remote.producer.writeFrame(bytes(7));
	remote.producer.finish();
	expect((await local.consumer.ordered().readFrame())?.payload).toEqual(bytes(7).payload);
});

test("a track session skips dropped groups and keeps the next message", async () => {
	const clock = time();
	const { local, remote } = await track();
	const reader = remote.consumer.ordered();
	const reading = reader.readFrame();
	hooks.dropTrackGroups(local.producer, 0n, 1n, { error: new NotFound("message unavailable") });
	const group = local.producer.createGroup(1n);
	group.writeFrame(bytes(7));
	group.finish();
	local.producer.finish();
	await clock.advance(Duration.ZERO);
	const frame = await reading;
	expect(frame?.group).toBe(1n);
	expect(frame?.payload).toEqual(bytes(7).payload);
	expect(await reader.readFrame()).toBeUndefined();
});

test("concurrent sessions between the same endpoints never reuse an identity", async () => {
	const { a, b } = setup();
	const nextFirst = b.acceptTrack();
	const nextSecond = b.acceptTrack();
	const dialingFirst = a.dialTrack("bob");
	const dialingSecond = a.dialTrack("bob");
	const [incomingFirst, incomingSecond] = await Promise.all([nextFirst, nextSecond]);
	expect(incomingFirst.session).not.toBe(incomingSecond.session);
	const [remoteFirst, remoteSecond, localFirst, localSecond] = await Promise.all([
		incomingFirst.accept(),
		incomingSecond.accept(),
		dialingFirst,
		dialingSecond,
	]);
	const remotes = new Map([
		[remoteFirst.session, remoteFirst],
		[remoteSecond.session, remoteSecond],
	]);
	localFirst.producer.writeFrame(bytes(1));
	localSecond.producer.writeFrame(bytes(2));
	const first = remotes.get(localFirst.session);
	const second = remotes.get(localSecond.session);
	expect((await first?.consumer.ordered().readFrame())?.payload).toEqual(bytes(1).payload);
	expect((await second?.consumer.ordered().readFrame())?.payload).toEqual(bytes(2).payload);
});

test("finished track sessions release their unlisted producers after both halves end", async () => {
	const clock = time();
	const { local, remote } = await track();
	const localReader = local.consumer.ordered();
	const remoteReader = remote.consumer.ordered();
	local.producer.writeFrame(bytes(1));
	remote.producer.writeFrame(bytes(2));
	local.producer.finish();
	remote.producer.finish();
	expect((await localReader.readFrame())?.payload).toEqual(bytes(2).payload);
	expect((await remoteReader.readFrame())?.payload).toEqual(bytes(1).payload);
	expect(await localReader.readFrame()).toBeUndefined();
	expect(await remoteReader.readFrame()).toBeUndefined();
	await clock.advance(Duration.ZERO);
	expect(hooks.trackReleased(local.producer).peek()).toBeNull();
	expect(hooks.trackReleased(remote.producer).peek()).toBeNull();
});

test("late reads retain return-routability checks after the original dial closes", async () => {
	const clock = time();
	const { origin, a, b, local, remote } = await track();
	const next = b.acceptTrack();
	void next.catch(() => {});
	const reset = new StreamError(StreamCode(70));
	const oldReader = local.consumer.ordered();
	remote.producer.abort(reset);
	await expect(oldReader.readFrame()).rejects.toBeInstanceOf(Error);
	const source = await broadcast(origin, ".server", "bob");
	let outcome: unknown;
	const fetched = source
		.track(`track/alice/bob/${local.session}`)
		.fetchGroup(0n)
		.then(
			(value) => {
				outcome = value;
			},
			(error) => {
				outcome = error;
			},
		);
	try {
		await clock.advance(Duration.ZERO);
		const unverified = await next;
		expect(unverified.session).toBe(local.session);
		// A late query has the old name, but the original dial no longer answers.
		// Verification must fail before a new producer can publish under that identity.
		await expect(unverified.accept()).rejects.toBeInstanceOf(NotFound);
		await fetched;
		expect(outcome).toBeInstanceOf(Error);
		const freshIncoming = b.acceptTrack();
		const dialing = a.dialTrack("bob");
		const incoming = await freshIncoming;
		expect(incoming.session).not.toBe(local.session);
		const remoteFresh = await incoming.accept();
		const localFresh = await dialing;
		localFresh.producer.writeFrame(bytes(9));
		const frame = await remoteFresh.consumer.ordered().readFrame();
		expect(frame?.payload).toEqual(bytes(9).payload);
		await expect(oldReader.readFrame()).rejects.toBeInstanceOf(Error);
	} finally {
		a.close();
		b.close();
		await fetched;
	}
});

test("an endpoint announces only the roles it takes, and its client answers only its reserved sessions", async () => {
	const { origin, a, b } = setup();
	await expect(a.dialTrack("alice")).rejects.toBeInstanceOf(Error);
	expect(announced(origin, ".client", "alice")).toBeUndefined();
	const cancel = new AbortController();
	const dialing = a.dialTrack("bob", undefined, { signal: cancel.signal });
	const failure = dialing.catch((error: unknown) => error);
	expect(announced(origin, ".client", "alice")).toBeDefined();
	expect(announced(origin, ".server", "alice")).toBeUndefined();
	const unsupported = await raw(origin, ".client", "alice", "track/bob/alice/not-a-dial");
	await expect(unsupported.info()).rejects.toBeInstanceOf(NotFound);
	unsupported.close();

	// Bob has not accepted yet, so the dial waits for him to serve.
	const clock = time();
	await clock.advance(Duration.secs(3600));
	expect(announced(origin, ".server", "bob")).toBeUndefined();
	const incoming = b.acceptTrack();
	expect(announced(origin, ".server", "bob")).toBeDefined();
	expect(announced(origin, ".client", "bob")).toBeUndefined();
	expect((await incoming).peer).toBe("alice");
	cancel.abort();
	expect(await failure).toBeInstanceOf(Error);
});

test("dials in another protocol, tcp included, reset without entering the queue", async () => {
	const { origin, b } = setup();
	const hold = new AbortController();
	const incoming = b.acceptTrack({ signal: hold.signal });
	const rejected = incoming.catch((error: unknown) => error);
	for (const protocol of ["tcp", "quic"]) {
		const subscriber = await raw(origin, ".server", "bob", `${protocol}/alice/bob/session-${protocol}`);
		await expect(subscriber.info()).rejects.toBeInstanceOf(Error);
		subscriber.close();
	}
	hold.abort();
	expect(await rejected).toBeInstanceOf(Error);
});

test("reject refuses one dial and leaves another independent session usable", async () => {
	const { a, b } = setup();
	const incoming = b.acceptTrack();
	const refused = a.dialTrack("bob");
	const refusal = refused.catch((error: unknown) => error);
	(await incoming).reject();
	expect(await refusal).toBeInstanceOf(NotFound);
	const again = b.acceptTrack();
	const dialing = a.dialTrack("bob");
	const remote = await (await again).accept();
	const local = await dialing;
	local.producer.writeFrame(bytes(42));
	expect((await remote.consumer.ordered().readFrame())?.payload).toEqual(bytes(42).payload);
});

test("cancelled dials leave queues and cannot be accepted later", async () => {
	const clock = time();
	const { a, b } = setup();
	const next = b.acceptTrack();
	const cancel = new AbortController();
	const reason = new Error("gave up");
	const dialing = a.dialTrack("bob", undefined, { signal: cancel.signal });
	const failure = dialing.catch((error: unknown) => error);
	const incoming = await next;
	cancel.abort(reason);
	expect(await failure).toBe(reason);
	await expect(incoming.accept()).rejects.toBeInstanceOf(Error);
	// Queue a dial after the application stopped pulling, then abandon it.
	const pending = new AbortController();
	const dropped = a.dialTrack("bob", undefined, { signal: pending.signal });
	const observed = dropped.catch((error: unknown) => error);
	await clock.advance(Duration.ZERO);
	pending.abort();
	expect(await observed).toBeInstanceOf(Error);
	const newIncoming = b.acceptTrack();
	const fresh = a.dialTrack("bob");
	const accepted = await newIncoming;
	expect(accepted.session).not.toBe(incoming.session);
	await Promise.all([accepted.accept(), fresh]);
});

test("dial and accept cancellation before announcement release their routing holds", async () => {
	const clock = time();
	const { origin, a, b } = setup();
	const cancel = new AbortController();
	const dialing = a.dialTrack("later", undefined, { signal: cancel.signal });
	const failure = dialing.catch((error: unknown) => error);
	await clock.advance(Duration.secs(3600));
	cancel.abort();
	expect(await failure).toBeInstanceOf(Error);
	const next = b.acceptTrack();
	const subscriber = await raw(origin, ".server", "bob", "track/absent/bob/never-announced");
	const accepted = (await next).accept();
	const acceptanceFailure = accepted.catch((error: unknown) => error);
	await clock.advance(Duration.secs(3600));
	subscriber.close();
	expect(await acceptanceFailure).toBeInstanceOf(Error);
});

test("a dial waits for the peer to announce, without an arbitrary timeout", async () => {
	const clock = time();
	const { origin, a } = setup();
	const dialing = a.dialTrack("later");
	await clock.advance(Duration.secs(3600));
	const later = new Socket.Endpoint(origin, Path.from("room"), "later");
	const incoming = await later.acceptTrack();
	const remote = await incoming.accept();
	const local = await dialing;
	local.producer.writeFrame(bytes(1));
	expect((await remote.consumer.ordered().readFrame())?.payload).toEqual(bytes(1).payload);
});

test("accept waits for a delayed dialer announcement only while its original dial stays open", async () => {
	const clock = time();
	const { origin, b } = setup();
	const next = b.acceptTrack();
	const subscriber = await raw(origin, ".server", "bob", "track/late/bob/delayed-announcement");
	const incoming = await next;
	const acceptance = incoming.accept();
	await clock.advance(Duration.secs(3600));
	const route = origin.route(Path.from("room/.client/late"), { hidden: true });
	const answer = hooks.reserveUnlistedTrack(route.broadcast, "track/late/bob/delayed-announcement");
	route.announce();
	await clock.advance(Duration.ZERO);
	expect(answer.demand().wanted.peek()).toBeDefined();
	const producer = answer.accept({ maxAge: null });
	const remote = await acceptance;
	producer.writeFrame(bytes(3));
	expect((await remote.consumer.ordered().readFrame())?.payload).toEqual(bytes(3).payload);
	subscriber.close();
});

test("forged and replayed dials cannot pass return routability", async () => {
	const { origin, a, b } = setup();
	// Alice dials elsewhere, so her client broadcast is there to refuse bob's check.
	void a.dialTrack("carol").catch(() => {});
	for (const name of ["track/alice/bob/forged", "track/alice/someone-else/replayed"]) {
		const cancel = new AbortController();
		const wait = b.acceptTrack({ signal: cancel.signal });
		void wait.catch(() => {});
		const subscriber = await raw(origin, ".server", "bob", name);
		const refusal = subscriber.info().catch((error: unknown) => error);
		if (name.includes("/bob/")) await expect((await wait).accept()).rejects.toBeInstanceOf(NotFound);
		else cancel.abort();
		expect(await refusal).toBeInstanceOf(NotFound);
		subscriber.close();
	}
	a.close();
});

test("session names are absent from listings while reserved and established", async () => {
	const { origin, a, b } = setup();
	const next = b.acceptTrack();
	const dialing = a.dialTrack("bob");
	const incoming = await next;
	const endpoints = [
		[".client", "alice"],
		[".server", "bob"],
	] as const;
	for (const [role, id] of endpoints) {
		const listing = (await broadcast(origin, role, id)).tracks();
		expect(await listing.next()).toEqual({ kind: "synced" });
		listing.close();
	}
	const remote = await incoming.accept();
	await dialing;
	for (const [role, id] of endpoints) {
		const listing = (await broadcast(origin, role, id)).tracks();
		expect(await listing.next()).toEqual({ kind: "synced" });
		listing.close();
	}
	expect(remote.session).toBe(incoming.session);
});

test("a track reset or endpoint close fails both halves and leaves no open accepts", async () => {
	for (const action of ["abort", "endpoint"] as const) {
		const { a, b, local, remote } = await track();
		if (action === "abort") local.producer.abort(new StreamError(StreamCode(71)));
		else a.close();
		await expect(remote.consumer.ordered().readFrame()).rejects.toBeInstanceOf(Error);
		expect(await remote.producer.closed).toBeInstanceOf(Error);
		const pending = b.acceptTrack();
		const failure = pending.catch((error: unknown) => error);
		b.close();
		a.close();
		expect(await failure).toBeInstanceOf(Error);
	}
});

test("endpoint ids refuse separators, and close is idempotent", async () => {
	const { origin, a } = setup();
	for (const id of ["", "a/b", "a\\b"]) {
		expect(() => new Socket.Endpoint(origin, Path.from("room"), id)).toThrow();
		await expect(a.dialTrack(id)).rejects.toBeInstanceOf(Error);
	}
	a.close();
	a.close();
	await expect(a.acceptTrack()).rejects.toBeInstanceOf(Error);
	await expect(a.dialTrack("bob")).rejects.toBeInstanceOf(Error);
});

test(`a socket preserves an early answer burst over ${Lite.ALPN_07_WIP}`, async () => {
	const alpn = Lite.ALPN_07_WIP;
	const clock = time();
	const ao = new Origin.Producer();
	const bo = new Origin.Producer();
	origins.add(ao);
	origins.add(bo);
	const a = new Socket.Endpoint(ao, Path.from("room"), "alice");
	const route = bo.route(Path.from("room/.server/bob"), { hidden: true });
	const requested = wireOf(route.broadcast).requested();
	route.announce();
	const pair = createMockTransportPair(alpn);
	const url = new URL("https://localhost/socket");
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, publish: ao.consume(), consume: ao }),
		acceptConnection({ url, transport: pair.server, publish: bo.consume(), consume: bo }),
	]);
	let returning: Track.Subscriber | undefined;
	let drained: Promise<void> | undefined;
	const following = bo.follow(Path.from("room/.client/alice"));
	try {
		const dialing = a.dialTrack("bob");
		void dialing.catch(() => {});
		const request = await requested;
		if (!request) throw new Error("missing dial");
		let caller = following.active.peek();
		while (!caller) {
			await following.active.changed();
			caller = following.active.peek();
		}
		returning = caller.track(request.name).subscribe({
			order: "ascending",
			delay: { max: null },
			ranges: [{ start: { absolute: 0n } }],
		});
		await returning.info();
		// The raw peer commits metadata only after writing its entire half, so
		// SUBSCRIBE_OK carries the burst's last position, without a timing race.
		hooks.keepRequest(request);
		const staged = hooks.stageRequest(request, { maxAge: null });
		for (let i = 1; i <= 3; i++) staged.producer.writeFrame(bytes(i));
		staged.producer.finish();
		staged.commit();
		await clock.advance(Duration.ZERO);
		const local = await dialing;
		const reader = local.consumer.ordered();
		const received: number[] = [];
		let completed = false;
		let failure: unknown;
		drained = (async () => {
			for (let frame = await reader.readFrame(); frame; frame = await reader.readFrame())
				received.push(...frame.payload);
		})().then(
			() => {
				completed = true;
			},
			(error) => {
				failure = error;
			},
		);
		await clock.advance(Duration.ZERO);
		expect(failure).toBeUndefined();
		expect(received).toEqual([1, 2, 3]);
		expect(completed).toBe(true);
		local.producer.finish();
	} finally {
		returning?.close();
		following.close();
		a.close();
		route.broadcast.close();
		client.abort();
		server.abort();
		await drained;
	}
});

for (const alpn of [Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18, Ietf.ALPN.DRAFT_20, Ietf.ALPN.DRAFT_22]) {
	test(`a socket refusal reaches a routed dialer over ${alpn}`, async () => {
		const clock = time();
		const ao = new Origin.Producer();
		const bo = new Origin.Producer();
		origins.add(ao);
		origins.add(bo);
		const a = new Socket.Endpoint(ao, Path.from("room"), "alice");
		const b = new Socket.Endpoint(bo, Path.from("room"), "bob");
		const pair = createMockTransportPair(alpn);
		const url = new URL("https://localhost/socket");
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client, publish: ao.consume(), consume: ao }),
			acceptConnection({ url, transport: pair.server, publish: bo.consume(), consume: bo }),
		]);
		let outcome: unknown;
		const next = b.acceptTrack();
		const dialing = a.dialTrack("bob");
		const settled = dialing.then(
			(stream) => {
				outcome = stream;
			},
			(error) => {
				outcome = error;
			},
		);
		try {
			(await next).reject();
			await clock.advance(Duration.ZERO);
			expect(outcome).toBeInstanceOf(Error);
		} finally {
			a.close();
			b.close();
			client.abort();
			server.abort();
			await settled;
		}
	});
}

for (const alpn of [Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18, Ietf.ALPN.DRAFT_20, Ietf.ALPN.DRAFT_22]) {
	test(`a pending socket acceptance over ${alpn} has no arbitrary deadline`, async () => {
		const clock = time();
		const ao = new Origin.Producer();
		const bo = new Origin.Producer();
		origins.add(ao);
		origins.add(bo);
		const a = new Socket.Endpoint(ao, Path.from("room"), "alice");
		const b = new Socket.Endpoint(bo, Path.from("room"), "bob");
		const pair = createMockTransportPair(alpn);
		const url = new URL("https://localhost/socket");
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client, publish: ao.consume(), consume: ao }),
			acceptConnection({ url, transport: pair.server, publish: bo.consume(), consume: bo }),
		]);
		const next = b.acceptTrack();
		let outcome: unknown;
		const dialing = a.dialTrack("bob");
		const settled = dialing.then(
			(value) => {
				outcome = value;
			},
			(error) => {
				outcome = error;
			},
		);
		try {
			const incoming = await next;
			await clock.advance(Duration.secs(3600));
			expect(outcome).toBeUndefined();
			const remote = await incoming.accept();
			const local = await dialing;
			local.producer.writeFrame(bytes(7));
			const frame = await remote.consumer.ordered().readFrame();
			expect(frame?.payload).toEqual(bytes(7).payload);
		} finally {
			a.close();
			b.close();
			client.abort();
			server.abort();
			await settled;
		}
	});
}

// A moq-lite client session before 07 never carries the endpoint's hidden broadcast, so its peer
// could never find it. Its dials and accepts fail as unsupported instead of waiting, as in Rust.
test("a socket endpoint behind a lite-06 client fails its dials and accepts, not the server's", async () => {
	const clock = time();
	const ao = new Origin.Producer();
	const bo = new Origin.Producer();
	origins.add(ao);
	origins.add(bo);
	const a = new Socket.Endpoint(ao, Path.from("room"), "alice");
	const b = new Socket.Endpoint(bo, Path.from("room"), "bob");
	// Already waiting before the session attaches, as when an endpoint's client reconnects.
	const waiting = a.dialTrack("bob").then(
		() => undefined,
		(error: unknown) => error,
	);
	const pair = createMockTransportPair(Lite.ALPN_06);
	const url = new URL("https://localhost/socket");
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, publish: ao.consume(), consume: ao }),
		acceptConnection({ url, transport: pair.server, publish: bo.consume(), consume: bo }),
	]);
	let heard = false;
	const next = b.acceptTrack().then(
		() => {
			heard = true;
		},
		() => {},
	);
	try {
		const unsupported = /unreachable behind a moq-lite hop before 07/;
		expect(((await waiting) as Error).message).toMatch(unsupported);
		await expect(a.dialTrack("bob")).rejects.toThrow(unsupported);
		await expect(a.acceptTrack()).rejects.toThrow(unsupported);
		// The server's endpoint keeps accepting, and never heard a dial it can't answer.
		await clock.advance(Duration.secs(3600));
		expect(heard).toBe(false);

		// The session's end releases the endpoint: an accept waits again.
		client.abort();
		await client.closed;
		let accepted: unknown;
		const accepting = a.acceptTrack().then(
			(value) => {
				accepted = value;
			},
			(error: unknown) => {
				accepted = error;
			},
		);
		await clock.advance(Duration.ZERO);
		expect(accepted).toBeUndefined();
		a.close();
		await accepting;
	} finally {
		client.abort();
		server.abort();
		a.close();
		b.close();
		await next;
	}
});

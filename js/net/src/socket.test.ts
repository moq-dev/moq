import { afterEach, expect, test } from "bun:test";
import { Manual, Milli, Nano, Second } from "@moq/clock";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { accept as acceptConnection } from "./connection/accept.ts";
import { connect } from "./connection/connect.ts";
import { NotFound, ProtocolViolation, StreamCode, StreamError } from "./error.ts";
import * as Ietf from "./ietf/index.ts";
import { Origin, Path, socket } from "./index.ts";
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
	const a = new socket.Endpoint(origin, Path.from("room"), "alice");
	const b = new socket.Endpoint(origin, Path.from("room"), "bob");
	return { origin, a, b };
}
async function tcp() {
	const state = setup();
	const incoming = state.b.acceptTcp();
	const dialing = state.a.dialTcp("bob");
	const remote = await (await incoming).accept();
	return { ...state, local: await dialing, remote };
}
async function track() {
	const state = setup();
	const incoming = state.b.acceptTrack();
	const dialing = state.a.dialTrack("bob");
	const remote = await (await incoming).accept();
	return { ...state, local: await dialing, remote };
}
async function broadcast(origin: Origin.Producer, id: string): Promise<BroadcastConsumer> {
	const request = origin.request(Path.from("room/.socket", id));
	const active = request.active.peek();
	if (!active) throw new Error("endpoint not routed locally");
	return active;
}
async function raw(origin: Origin.Producer, id: string, name: string): Promise<Track.Subscriber> {
	return (await broadcast(origin, id)).track(name).subscribe({ order: "ascending", delay: { max: null } });
}

test("socket namespace exposes endpoint and both stream roles", () => {
	expect(socket.Endpoint).toBeFunction();
	expect(socket.TcpStream).toBeFunction();
	expect(socket.TrackStream).toBeFunction();
});

test("TCP carries ordered frames in both directions, with independent FIN", async () => {
	const { a, b, local, remote } = await tcp();
	expect(local.peer).toBe("bob");
	expect(remote.peer).toBe("alice");
	expect(local.session).toBe(remote.session);
	expect(local.session).toMatch(/^[0-9a-f]{32}$/);
	for (let i = 0; i < 3; i++) local.producer.writeFrame(bytes(i));
	local.producer.finish();
	for (let i = 0; i < 3; i++) expect((await remote.consumer.readFrame())?.payload).toEqual(bytes(i).payload);
	expect(await remote.consumer.readFrame()).toBeUndefined();
	remote.producer.writeFrame(bytes(9));
	remote.producer.finish();
	expect((await local.consumer.readFrame())?.payload).toEqual(bytes(9).payload);
	expect(await local.consumer.readFrame()).toBeUndefined();
	a.close();
	b.close();
});

test("a peer route replaced between metadata and opening cannot inherit a socket session", async () => {
	const clock = time();
	const { origin, a } = setup();
	const peer = new BroadcastProducer();
	const keep = peer.consume();
	const routes: { close(): void }[] = [];
	const tasks: Promise<void>[] = [];
	const advertise = (front: BroadcastConsumer) => {
		const route = wireOf(origin).receive(Path.from("room/.socket/remote"));
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
		returning = await raw(origin, "alice", request.name);
		await returning.info();
		request.accept({ maxAge: null });
		// Both route changes happen before the info promise's continuation opens the session.
		first.close();
		advertise(replacementKeep);
		await clock.advance(Nano.zero);
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
	expect((await local.producer.info()).maxAge).toBeUndefined();
	expect(remote.consumer.subscription.peek()?.order).toBe("ascending");
	expect(remote.consumer.subscription.peek()?.delay?.max).toBe(Milli(Number.MAX_SAFE_INTEGER));
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

test("each protocol has its own accept queue and sessions never reuse an identity", async () => {
	const { a, b } = setup();
	const nextTrack = b.acceptTrack();
	const nextTcp = b.acceptTcp();
	const dialingTcp = a.dialTcp("bob");
	const dialingTrack = a.dialTrack("bob");
	const [incomingTrack, incomingTcp] = await Promise.all([nextTrack, nextTcp]);
	expect(incomingTcp.session).not.toBe(incomingTrack.session);
	const [remoteTrack, remoteTcp, localTrack, localTcp] = await Promise.all([
		incomingTrack.accept(),
		incomingTcp.accept(),
		dialingTrack,
		dialingTcp,
	]);
	localTrack.producer.writeFrame(bytes(1));
	localTcp.producer.writeFrame(bytes(2));
	expect((await remoteTrack.consumer.ordered().readFrame())?.payload).toEqual(bytes(1).payload);
	expect((await remoteTcp.consumer.readFrame())?.payload).toEqual(bytes(2).payload);
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
	await clock.advance(Nano.zero);
	expect(hooks.trackReleased(local.producer).peek()).toBeNull();
	expect(hooks.trackReleased(remote.producer).peek()).toBeNull();
});

for (const protocol of ["tcp", "track"] as const) {
	test(`late ${protocol} reads retain return-routability checks after the original dial closes`, async () => {
		const clock = time();
		const { origin, a, b, local, remote } = await (protocol === "tcp" ? tcp() : track());
		const next = protocol === "tcp" ? b.acceptTcp() : b.acceptTrack();
		void next.catch(() => {});
		const reset = new StreamError(StreamCode(70));
		const oldReader = local instanceof socket.TcpStream ? local.consumer : local.consumer.ordered();
		remote.producer.abort(reset);
		await expect(oldReader.readFrame()).rejects.toBeInstanceOf(Error);
		const source = await broadcast(origin, "bob");
		let outcome: unknown;
		const fetched = source
			.track(`${protocol}/alice/bob/${local.session}`)
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
			await clock.advance(Nano.zero);
			const unverified = await next;
			expect(unverified.session).toBe(local.session);
			// A late query has the old name, but the original dial no longer answers.
			// Verification must fail before a new producer can publish under that identity.
			await expect(unverified.accept()).rejects.toBeInstanceOf(NotFound);
			await fetched;
			expect(outcome).toBeInstanceOf(Error);
			const freshIncoming = protocol === "tcp" ? b.acceptTcp() : b.acceptTrack();
			const dialing = protocol === "tcp" ? a.dialTcp("bob") : a.dialTrack("bob");
			const incoming = await freshIncoming;
			expect(incoming.session).not.toBe(local.session);
			const remoteFresh = await incoming.accept();
			const localFresh = await dialing;
			localFresh.producer.writeFrame(bytes(9));
			const frame =
				remoteFresh instanceof socket.TcpStream
					? await remoteFresh.consumer.readFrame()
					: await remoteFresh.consumer.ordered().readFrame();
			expect(frame?.payload).toEqual(bytes(9).payload);
			await expect(oldReader.readFrame()).rejects.toBeInstanceOf(Error);
		} finally {
			a.close();
			b.close();
			await fetched;
		}
	});
}

test("a dial-only endpoint refuses incoming requests and answers only its reserved session", async () => {
	const { origin, a } = setup();
	await expect(a.dialTcp("alice")).rejects.toBeInstanceOf(Error);
	const unsupported = await raw(origin, "alice", "tcp/bob/alice/not-a-dial");
	await expect(unsupported.info()).rejects.toBeInstanceOf(NotFound);
	unsupported.close();
});

test("unknown and unaccepted protocols reset without entering the other queue", async () => {
	const { origin, b } = setup();
	const hold = new AbortController();
	const incoming = b.acceptTcp({ signal: hold.signal });
	const rejected = incoming.catch((error: unknown) => error);
	for (const protocol of ["track", "quic"]) {
		const subscriber = await raw(origin, "bob", `${protocol}/alice/bob/session-${protocol}`);
		await expect(subscriber.info()).rejects.toBeInstanceOf(Error);
		subscriber.close();
	}
	hold.abort();
	expect(await rejected).toBeInstanceOf(Error);
});

test("reject refuses one dial and leaves another independent session usable", async () => {
	const { a, b } = setup();
	const incoming = b.acceptTcp();
	const refused = a.dialTcp("bob");
	const refusal = refused.catch((error: unknown) => error);
	(await incoming).reject();
	expect(await refusal).toBeInstanceOf(NotFound);
	const again = b.acceptTcp();
	const dialing = a.dialTcp("bob");
	const remote = await (await again).accept();
	const local = await dialing;
	local.producer.writeFrame(bytes(42));
	expect((await remote.consumer.readFrame())?.payload).toEqual(bytes(42).payload);
});

test("cancelled dials leave queues and cannot be accepted later", async () => {
	const clock = time();
	const { a, b } = setup();
	const next = b.acceptTcp();
	const cancel = new AbortController();
	const reason = new Error("gave up");
	const dialing = a.dialTcp("bob", { signal: cancel.signal });
	const failure = dialing.catch((error: unknown) => error);
	const incoming = await next;
	cancel.abort(reason);
	expect(await failure).toBe(reason);
	await expect(incoming.accept()).rejects.toBeInstanceOf(Error);
	// Queue a dial after the application stopped pulling, then abandon it.
	const pending = new AbortController();
	const dropped = a.dialTcp("bob", { signal: pending.signal });
	const observed = dropped.catch((error: unknown) => error);
	await clock.advance(Nano.zero);
	pending.abort();
	expect(await observed).toBeInstanceOf(Error);
	const newIncoming = b.acceptTcp();
	const fresh = a.dialTcp("bob");
	const accepted = await newIncoming;
	expect(accepted.session).not.toBe(incoming.session);
	await Promise.all([accepted.accept(), fresh]);
});

test("dial and accept cancellation before announcement release their routing holds", async () => {
	const clock = time();
	const { origin, a, b } = setup();
	const cancel = new AbortController();
	const dialing = a.dialTcp("later", { signal: cancel.signal });
	const failure = dialing.catch((error: unknown) => error);
	await clock.advance(Nano.fromSecond(Second(3600)));
	cancel.abort();
	expect(await failure).toBeInstanceOf(Error);
	const next = b.acceptTcp();
	const subscriber = await raw(origin, "bob", "tcp/absent/bob/never-announced");
	const accepted = (await next).accept();
	const acceptanceFailure = accepted.catch((error: unknown) => error);
	await clock.advance(Nano.fromSecond(Second(3600)));
	subscriber.close();
	expect(await acceptanceFailure).toBeInstanceOf(Error);
});

test("a dial waits for the peer to announce, without an arbitrary timeout", async () => {
	const clock = time();
	const { origin, a } = setup();
	const dialing = a.dialTcp("later");
	await clock.advance(Nano.fromSecond(Second(3600)));
	const later = new socket.Endpoint(origin, Path.from("room"), "later");
	const incoming = await later.acceptTcp();
	const remote = await incoming.accept();
	const local = await dialing;
	local.producer.writeFrame(bytes(1));
	expect((await remote.consumer.readFrame())?.payload).toEqual(bytes(1).payload);
});

test("accept waits for a delayed dialer announcement only while its original dial stays open", async () => {
	const clock = time();
	const { origin, b } = setup();
	const next = b.acceptTrack();
	const subscriber = await raw(origin, "bob", "track/late/bob/delayed-announcement");
	const incoming = await next;
	const acceptance = incoming.accept();
	await clock.advance(Nano.fromSecond(Second(3600)));
	const publication = origin.createBroadcast(Path.from("room/.socket/late"));
	const answer = hooks.reserveUnlistedTrack(publication.broadcast, "track/late/bob/delayed-announcement");
	publication.announce();
	await clock.advance(Nano.zero);
	expect(answer.demand().wanted.peek()).toBeDefined();
	const producer = answer.accept({ maxAge: null });
	const remote = await acceptance;
	producer.writeFrame(bytes(3));
	expect((await remote.consumer.ordered().readFrame())?.payload).toEqual(bytes(3).payload);
	subscriber.close();
});

test("forged and replayed dials cannot pass return routability", async () => {
	const { origin, a, b } = setup();
	for (const name of ["tcp/alice/bob/forged", "tcp/alice/someone-else/replayed"]) {
		const cancel = new AbortController();
		const wait = b.acceptTcp({ signal: cancel.signal });
		void wait.catch(() => {});
		const subscriber = await raw(origin, "bob", name);
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
	const next = b.acceptTcp();
	const dialing = a.dialTcp("bob");
	const incoming = await next;
	for (const id of ["alice", "bob"]) {
		const listing = (await broadcast(origin, id)).tracks();
		expect(await listing.next()).toEqual({ kind: "synced" });
		listing.close();
	}
	const remote = await incoming.accept();
	await dialing;
	for (const id of ["alice", "bob"]) {
		const listing = (await broadcast(origin, id)).tracks();
		expect(await listing.next()).toEqual({ kind: "synced" });
		listing.close();
	}
	expect(remote.session).toBe(incoming.session);
});

test("TCP RST, reader cancellation, and endpoint lifetime reset both halves", async () => {
	for (const action of ["abort", "reader", "endpoint"] as const) {
		const { a, local, remote } = await tcp();
		const reset = new StreamError(StreamCode(70));
		if (action === "abort") local.producer.abort(reset);
		if (action === "reader") local.consumer.close();
		if (action === "endpoint") a.close();
		await expect(remote.consumer.readFrame()).rejects.toBeInstanceOf(Error);
		expect(await remote.producer.closed).toBeInstanceOf(Error);
	}
});

test("track reset and endpoint close fail peers and leave no open accepts", async () => {
	const { a, b, local, remote } = await track();
	local.producer.abort(new StreamError(StreamCode(71)));
	await expect(remote.consumer.ordered().readFrame()).rejects.toBeInstanceOf(Error);
	expect(await remote.producer.closed).toBeInstanceOf(Error);
	const pending = b.acceptTcp();
	const failure = pending.catch((error: unknown) => error);
	b.close();
	a.close();
	expect(await failure).toBeInstanceOf(Error);
});

test("a second TCP group resets the session even after the first FIN", async () => {
	for (const initial of [0n, 2n]) {
		const { origin, b } = setup();
		const publication = origin.createBroadcast(Path.from("room/.socket/raw"));
		publication.announce();
		const name = `tcp/raw/bob/second-${initial}`;
		const answer = hooks.reserveUnlistedTrack(publication.broadcast, name);
		const next = b.acceptTcp();
		const subscriber = await raw(origin, "bob", name);
		const acceptance = (await next).accept();
		// Drive the accept's return subscription before publishing the first half.
		const clock = time();
		await clock.advance(Nano.zero);
		const producer = answer.accept();
		const stream = await acceptance;
		const group = producer.createGroup(initial);
		group.writeFrame(bytes(1));
		group.finish();
		if (initial === 0n) {
			expect((await stream.consumer.readFrame())?.payload).toEqual(bytes(1).payload);
			expect(await stream.consumer.readFrame()).toBeUndefined();
			producer.createGroup(1n).finish();
		}
		expect(await stream.producer.closed).toBeInstanceOf(ProtocolViolation);
		subscriber.close();
	}
});

test("unpaced TCP eviction is an error, never a skipped frame or clean FIN", async () => {
	const clock = time();
	const { local, remote } = await tcp();
	// The local group's bounded tail is 32 MiB. Every payload is a distinct frame identity.
	const payload = new Uint8Array(1024 * 1024);
	for (let i = 0; i < 32; i++) {
		local.producer.writeFrame({ payload });
		await clock.advance(Nano.zero);
	}
	local.producer.finish();
	await expect(remote.consumer.readFrame()).rejects.toMatchObject({ code: StreamCode.Evicted });
});

test("endpoint ids refuse separators, and close is idempotent", async () => {
	const { origin, a } = setup();
	for (const id of ["", "a/b", "a\\b"]) {
		expect(() => new socket.Endpoint(origin, Path.from("room"), id)).toThrow();
		await expect(a.dialTcp(id)).rejects.toBeInstanceOf(Error);
	}
	a.close();
	a.close();
	await expect(a.acceptTcp()).rejects.toBeInstanceOf(Error);
	await expect(a.dialTrack("bob")).rejects.toBeInstanceOf(Error);
});

for (const alpn of [Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18, Ietf.ALPN.DRAFT_20, Ietf.ALPN.DRAFT_22]) {
	for (const protocol of ["tcp", "track"] as const) {
		test(`socket ${protocol} preserves an early answer burst over ${alpn}`, async () => {
			const clock = time();
			const ao = new Origin.Producer();
			const bo = new Origin.Producer();
			origins.add(ao);
			origins.add(bo);
			const a = new socket.Endpoint(ao, Path.from("room"), "alice");
			const publication = bo.createBroadcast(Path.from("room/.socket/bob"));
			const requested = wireOf(publication.broadcast).requested();
			publication.announce();
			const pair = createMockTransportPair(alpn);
			const url = new URL("https://localhost/socket");
			const [client, server] = await Promise.all([
				connect({ url, transport: pair.client, publish: ao.consume(), consume: ao }),
				acceptConnection({ url, transport: pair.server, publish: bo.consume(), consume: bo }),
			]);
			let returning: Track.Subscriber | undefined;
			let drained: Promise<void> | undefined;
			const following = bo.follow(Path.from("room/.socket/alice"));
			try {
				const dialing = protocol === "tcp" ? a.dialTcp("bob") : a.dialTrack("bob");
				void dialing.catch(() => {});
				const request = await requested;
				if (!request) throw new Error("missing dial");
				let caller = following.active.peek();
				while (!caller) {
					await following.active.changed();
					caller = following.active.peek();
				}
				returning = caller
					.track(request.name)
					.subscribe({ order: "ascending", delay: { max: null }, ranges: [{ start: { absolute: 0n } }] });
				await returning.info();
				// The raw peer commits metadata only after writing its entire half, so
				// SUBSCRIBE_OK carries the burst's last position, without a timing race.
				hooks.keepRequest(request);
				const staged = hooks.stageRequest(request, { maxAge: null });
				if (protocol === "tcp") {
					const group = staged.producer.createGroup(0n);
					for (let i = 1; i <= 3; i++) group.writeFrame(bytes(i));
					group.finish();
				} else {
					for (let i = 1; i <= 3; i++) staged.producer.writeFrame(bytes(i));
				}
				staged.producer.finish();
				staged.commit();
				await clock.advance(Nano.zero);
				const local = await dialing;
				const reader = local instanceof socket.TcpStream ? local.consumer : local.consumer.ordered();
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
				await clock.advance(Nano.zero);
				expect(failure).toBeUndefined();
				expect(received).toEqual([1, 2, 3]);
				expect(completed).toBe(true);
				local.producer.finish();
			} finally {
				returning?.close();
				following.close();
				a.close();
				publication.broadcast.close();
				client.abort();
				server.abort();
				await drained;
			}
		});
	}
}

for (const alpn of [Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18, Ietf.ALPN.DRAFT_20, Ietf.ALPN.DRAFT_22]) {
	for (const protocol of ["tcp", "track"] as const) {
		test(`socket ${protocol} refusal reaches a routed dialer over ${alpn}`, async () => {
			const clock = time();
			const ao = new Origin.Producer();
			const bo = new Origin.Producer();
			origins.add(ao);
			origins.add(bo);
			const a = new socket.Endpoint(ao, Path.from("room"), "alice");
			const b = new socket.Endpoint(bo, Path.from("room"), "bob");
			const pair = createMockTransportPair(alpn);
			const url = new URL("https://localhost/socket");
			const [client, server] = await Promise.all([
				connect({ url, transport: pair.client, publish: ao.consume(), consume: ao }),
				acceptConnection({ url, transport: pair.server, publish: bo.consume(), consume: bo }),
			]);
			let outcome: unknown;
			const next = protocol === "tcp" ? b.acceptTcp() : b.acceptTrack();
			const dialing = protocol === "tcp" ? a.dialTcp("bob") : a.dialTrack("bob");
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
				await clock.advance(Nano.zero);
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
}

for (const alpn of [Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18, Ietf.ALPN.DRAFT_20, Ietf.ALPN.DRAFT_22]) {
	for (const protocol of ["tcp", "track"] as const) {
		test(`pending socket ${protocol} acceptance over ${alpn} has no arbitrary deadline`, async () => {
			const clock = time();
			const ao = new Origin.Producer();
			const bo = new Origin.Producer();
			origins.add(ao);
			origins.add(bo);
			const a = new socket.Endpoint(ao, Path.from("room"), "alice");
			const b = new socket.Endpoint(bo, Path.from("room"), "bob");
			const pair = createMockTransportPair(alpn);
			const url = new URL("https://localhost/socket");
			const [client, server] = await Promise.all([
				connect({ url, transport: pair.client, publish: ao.consume(), consume: ao }),
				acceptConnection({ url, transport: pair.server, publish: bo.consume(), consume: bo }),
			]);
			const next = protocol === "tcp" ? b.acceptTcp() : b.acceptTrack();
			let outcome: unknown;
			const dialing = protocol === "tcp" ? a.dialTcp("bob") : a.dialTrack("bob");
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
				await clock.advance(Nano.fromSecond(Second(3600)));
				expect(outcome).toBeUndefined();
				const remote = await incoming.accept();
				const local = await dialing;
				local.producer.writeFrame(bytes(7));
				const frame =
					remote instanceof socket.TcpStream
						? await remote.consumer.readFrame()
						: await remote.consumer.ordered().readFrame();
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
}

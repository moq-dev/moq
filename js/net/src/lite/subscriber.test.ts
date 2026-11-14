import { expect, spyOn, test } from "bun:test";
import { Duration, Elapsed, Manual, timeout, yieldNow } from "@moq/clock";
import { type Getter, Signal } from "@moq/signals";
import type { RouteOptions } from "../announced.ts";
import type { Grant } from "../auth.ts";
import type { AuthSession } from "../auth_session.ts";
import { Viewers } from "../bandwidth.ts";
import { DEFAULT_EXPIRY } from "../cache.ts";
import type { Probe as ProbeStats } from "../connection/stats.ts";
import {
	error,
	fromTransport,
	ProtocolViolation,
	reason,
	SessionCode,
	SessionError,
	Skipped,
	StreamCode,
	StreamError,
	TimestampMismatch,
	toTransport,
} from "../error.ts";
import type { Consumer as GroupConsumer } from "../group.ts";
import { type Hop, HopSchema, isAnonymous, MAX_HOPS, UNKNOWN_HOP, WireRoute } from "../hop.ts";
import * as Instance from "../instance.ts";
import { hooks, rendezvousHash } from "../internal.ts";
import { GRACE } from "../ledger.ts";
import * as Mint from "../mint.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import type { Producer } from "../track.ts";
import { SEGMENT_BYTES } from "../util/limits.ts";
import { Budget } from "../util/yield.ts";
import * as Varint from "../varint.ts";
import {
	type AnnounceBroadcast,
	AnnounceInit,
	AnnounceOk,
	AnnounceRequest,
	decodeAnnounceReplyMaybe,
	encodeAnnounceBroadcast,
	ROUTE_MAX_HOPS,
} from "./announce.ts";
import { Group } from "./group.ts";
import { Probe } from "./probe.ts";
import { ProbeLevel, Setup } from "./setup.ts";
import { StreamId } from "./stream.ts";
import {
	encodeSubscribeResponse,
	Subscribe,
	SubscribeDrop,
	SubscribeFinish,
	SubscribeOk,
	type SubscribeResponse,
	SubscribeSkip,
	SubscribeStart,
	SubscribeStarted,
	SubscribeUpdate,
} from "./subscribe.ts";
import { Minted, Subscriber } from "./subscriber.ts";
import { Track, TrackInfo } from "./track.ts";
import { hasLengthFirst, Version, versionName } from "./version.ts";

test("closing the subscriber suppresses probe stream warnings", async () => {
	let readable!: ReadableStreamDefaultController<Uint8Array>;
	const quic = {
		createBidirectionalStream: async () => ({
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (readable = controller) }),
			writable: new WritableStream<Uint8Array>(),
		}),
	} as unknown as WebTransport;
	const subscriber = new Subscriber(quic, Version.DRAFT_03, HopSchema.parse(1n), new Signal<ProbeStats>({}));
	const warn = spyOn(console, "warn").mockImplementation(() => {});

	try {
		const probe = subscriber.runProbe();

		await Promise.resolve();
		await Promise.resolve();
		subscriber.close();
		readable.error(new Error("session closed"));
		await probe;

		expect(warn).not.toHaveBeenCalled();
	} finally {
		warn.mockRestore();
	}
});

// Drives a Subscriber's announce stream directly: the harness plays the peer, writing
// forged announce messages into the stream the subscriber opens.
function announceHarness(version: Version, origin = 1n) {
	const reading = Promise.withResolvers<void>();
	let inbound!: ReadableStreamDefaultController<Uint8Array>;
	// Resolves with the reason once the subscriber resets the stream, which is how a peer
	// learns it violated the protocol. Never resolving is the failure this observes.
	let onAbort!: (reason: unknown) => void;
	const aborted = new Promise<unknown>((resolve) => (onAbort = resolve));
	// Resolves once the subscriber ends the session, which is what the draft requires of a
	// protocol violation: a stream reset alone would let the peer repeat it on the next one.
	let onSessionClose!: (info?: WebTransportCloseInfo) => void;
	const sessionClosed = new Promise<WebTransportCloseInfo | undefined>((resolve) => (onSessionClose = resolve));
	const quic = {
		createBidirectionalStream: async () => ({
			// A pull with no read-ahead means the request writes have finished.
			readable: new ReadableStream<Uint8Array>(
				{
					start: (controller) => (inbound = controller),
					pull: () => reading.resolve(),
				},
				{ highWaterMark: 0 },
			),
			writable: new WritableStream<Uint8Array>({ abort: (reason) => void onAbort(reason) }),
		}),
		close: (info?: WebTransportCloseInfo) => {
			// WebTransport throws rather than closing when the reason exceeds 1024 bytes of
			// UTF-8. Enforced here so an unbounded reason fails as a session that stayed up,
			// which is what it does in a browser, rather than as a long string.
			if (new TextEncoder().encode(info?.reason ?? "").byteLength > 1024) {
				throw new TypeError("close reason exceeds 1024 bytes");
			}
			onSessionClose(info);
		},
	} as unknown as WebTransport;

	const subscriber = new Subscriber(quic, version, HopSchema.parse(origin));

	const send = async (f: (w: Writer) => Promise<void>) => {
		const written: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
			version,
		);
		await f(writer);
		writer.close();
		await writer.closed;

		const total = written.reduce((sum, c) => sum + c.byteLength, 0);
		const out = new Uint8Array(total);
		let offset = 0;
		for (const chunk of written) {
			out.set(chunk, offset);
			offset += chunk.byteLength;
		}
		inbound.enqueue(out);
	};

	// The reason the subscriber reset the stream, or a prompt rejection if it never did.
	// A peer that is not told about its own violation should fail the test visibly rather
	// than hang it out to the suite timeout.
	const abortReason = async () => {
		try {
			return reason(error(await timeout(Duration.millis(250), aborted)));
		} catch (error) {
			if (error instanceof Elapsed) throw new Error("stream was never aborted");
			throw error;
		}
	};

	// Rejects promptly if the session outlives the violation, rather than hanging the test.
	const sessionEnded = async () => {
		try {
			return await timeout(Duration.millis(250), sessionClosed);
		} catch (error) {
			if (error instanceof Elapsed) throw new Error("session was never closed");
			throw error;
		}
	};

	return {
		subscriber,
		send,
		// The peer FINs the announce stream.
		finish: () => inbound.close(),
		abortReason,
		sessionEnded,
		settle: async () => {
			await reading.promise;
			await yieldNow();
		},
	};
}

const PUBLISHER_A = HopSchema.parse(7n);
const PUBLISHER_B = HopSchema.parse(8n);
const PUBLISHER_C = HopSchema.parse(9n);
const PEER = HopSchema.parse(2n);

// Lite-07 origin nodes, and the route a peer advertises to each.
const NODE_A = 0x7an;
const NODE_B = 0x8bn;
const ADVERT = { seqno: 1n, metric: 0n, downOnly: false, rtt: 20n, hops: [] as bigint[] };

/**
 * Write a claim the way `version` carries it: before lite-07 with its hop chain, and on lite-07
 * under a route of its own to `node`, closed by its floor. Route IDs count from 0 per stream, so
 * one helper serves one stream.
 */
function claimer(version: Version) {
	let routes = 0n;
	return async (
		w: Writer,
		suffix: Path.Valid,
		{ instance, hops, node }: { instance?: Instance.Valid; hops: Hop[]; node: bigint },
	) => {
		if (version !== Version.DRAFT_07) {
			await encodeAnnounceBroadcast(w, { status: "active", suffix, instance, hops }, version);
			return;
		}
		const route = routes++;
		await encodeAnnounceBroadcast(w, { status: "routeStart", node, advert: ADVERT }, version);
		await encodeAnnounceBroadcast(w, { status: "active", suffix, route, instance }, version);
		await encodeAnnounceBroadcast(w, { status: "floor", id: route }, version);
	};
}

test("an announce consumer closed while its stream opens sends nothing on it", async () => {
	const opening = Promise.withResolvers<void>();
	let written = 0;
	let closed = false;
	const quic = {
		createBidirectionalStream: async () => {
			await opening.promise;
			return {
				readable: new ReadableStream<Uint8Array>(),
				writable: new WritableStream<Uint8Array>({
					write: () => void written++,
					close: () => {
						closed = true;
					},
				}),
			};
		},
	} as unknown as WebTransport;
	const subscriber = new Subscriber(quic, Version.DRAFT_06, HopSchema.parse(1n));

	const announced = subscriber.routes(Path.Pattern.subtree(Path.from("room")));
	announced.close();
	// The slot frees up only after the consumer left.
	opening.resolve();
	await yieldNow();
	expect(written).toBe(0);
	expect(closed).toBe(true);
});

test("a local empty hop chain is not anonymous", () => {
	expect(isAnonymous(WireRoute.default)).toBe(false);
	expect(isAnonymous({ hops: [UNKNOWN_HOP], cost: WireRoute.default.cost })).toBe(true);
});

test("a max-length chain plus withheld responder is dropped", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();

	const hops = Array.from({ length: MAX_HOPS }, (_, i) => HopSchema.parse(BigInt(i + 1)));
	await send((w) => new AnnounceOk(UNKNOWN_HOP, 0).encode(w, Version.DRAFT_06));
	await send((w) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("full"), hops }, Version.DRAFT_06),
	);
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("room"),
		kind: "start",
		route: { hops: [PUBLISHER_A, UNKNOWN_HOP] },
	});

	announced.close();
	subscriber.close();
});

test("an unidentified responder keeps hop 0 on a nonempty chain", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(UNKNOWN_HOP, 0).encode(w, Version.DRAFT_06));
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("room"),
		kind: "start",
		route: { hops: [PUBLISHER_A, UNKNOWN_HOP] },
	});

	announced.close();
	subscriber.close();
});

test("a received hop list naming no publisher stays anonymous", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(UNKNOWN_HOP, 0).encode(w, Version.DRAFT_06));
	await send((w) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room"), hops: [] }, Version.DRAFT_06),
	);
	const update = await announced.next();
	expect(update).toMatchObject({ prefix: Path.from("room"), kind: "start", route: { hops: [UNKNOWN_HOP] } });
	expect(update && isAnonymous(update.route)).toBe(true);

	announced.close();
	subscriber.close();
});

test("a received chain starting with hop 0 is forwarded unchanged", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [UNKNOWN_HOP, PUBLISHER_A] },
			Version.DRAFT_06,
		),
	);
	const update = await announced.next();
	expect(update).toMatchObject({ prefix: Path.from("room"), kind: "start" });
	expect(update?.route.hops).toEqual([UNKNOWN_HOP, PUBLISHER_A, PEER]);

	announced.close();
	subscriber.close();
});

test("an update moves the route in place, even from another publisher", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();
	const room = Path.from("room");

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));
	await send((w) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: room, hops: [PUBLISHER_A] }, Version.DRAFT_06),
	);
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "start" });
	const held = subscriber.consume(room);

	// Same publisher over an identical route: no metadata to forward, so nothing surfaces.
	await send((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_06));

	// A reprice from the same publisher surfaces, which proves the identical update above
	// emitted nothing. The same publisher keeps sharing one broadcast.
	await send((w) =>
		encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [PUBLISHER_A], cost: 4n }, Version.DRAFT_06),
	);
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "update" });
	const same = subscriber.consume(room);
	expect(same.closed).toBe(held.closed);

	// A different publisher took the path: an in-place update, never a retraction.
	await send((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [PUBLISHER_B] }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({
		prefix: room,
		kind: "update",
		route: { hops: [PUBLISHER_B, PEER] },
	});

	// The path still names the same broadcast, so the next consume shares it.
	expect(subscriber.consume(room).closed).toBe(held.closed);
	expect(held.closed.peek()).toBeUndefined();

	// A third publisher is another update, still the same broadcast.
	await send((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [PUBLISHER_C] }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({
		prefix: room,
		kind: "update",
		route: { hops: [PUBLISHER_C, PEER] },
	});
	expect(subscriber.consume(room).closed).toBe(held.closed);

	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "end" });

	announced.close();
	subscriber.close();
});

test("a lite-07 restart replaces the instance, and the next consume subscribes fresh", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_07);
	const announced = subscriber.routes();
	await settle();
	const room = Path.from("room");

	const v = Version.DRAFT_07;
	await send((w) => new AnnounceOk(PEER, 0).encode(w, v));
	const first = Instance.random();
	await send((w) => claimer(v)(w, room, { instance: first, hops: [], node: NODE_A }));
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "start", route: { instance: first } });
	const held = subscriber.consume(room);

	// Another instance: a restart, and the handle already out stays.
	for (const instance of [Instance.random(), Instance.random()]) {
		await send(async (w) => {
			await encodeAnnounceBroadcast(w, { status: "restart", id: 0n, instance, cost: 3n }, v);
			await encodeAnnounceBroadcast(w, { status: "floor", id: 0n }, v);
		});
		const restarted = await announced.next();
		// Priced past the route's metric and this session's link.
		expect(restarted).toMatchObject({ prefix: room, kind: "restart", route: { cost: 4n } });
		expect(restarted?.route.instance).toBe(instance);
		const fresh = subscriber.consume(room);
		expect(fresh.closed).not.toBe(held.closed);
		expect(held.closed.peek()).toBeUndefined();
		fresh.close();
	}

	await send(async (w) => {
		await encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, v);
		// Certified, the claim's origin withdrew it: it ends rather than going offline.
		await encodeAnnounceBroadcast(w, { status: "floor", id: 0n, revision: 0n }, v);
	});
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "end" });

	held.close();
	announced.close();
	subscriber.close();
});

// A replaced prefix served everything consumed beneath it, so those paths subscribe fresh too,
// whether the replacement arrives as a lite-07 restart or as an end then a start.
test.each([
	["a lite-07 restart", Version.DRAFT_07],
	["an end then a start", Version.DRAFT_06],
] as const)("%s of a prefix stops sharing what was consumed beneath it", async (_, version) => {
	const { subscriber, send, settle } = announceHarness(version);
	const announced = subscriber.routes();
	await settle();
	const pool = Path.from("pool");
	const job = Path.from("pool/job");

	await send((w) => new AnnounceOk(PEER, 0).encode(w, version));
	await send((w) => claimer(version)(w, pool, { instance: named(version, "a"), hops: [PUBLISHER_A], node: NODE_A }));
	expect(await announced.next()).toMatchObject({ prefix: pool, kind: "start" });
	const held = subscriber.consume(job);

	if (version === Version.DRAFT_07) {
		await send(async (w) => {
			await encodeAnnounceBroadcast(w, { status: "restart", id: 0n, instance: named(version, "b") }, version);
			await encodeAnnounceBroadcast(w, { status: "floor", id: 0n }, version);
		});
		expect(await announced.next()).toMatchObject({ prefix: pool, kind: "restart" });
	} else {
		await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, version));
		expect(await announced.next()).toMatchObject({ prefix: pool, kind: "end" });
		await send((w) => encodeAnnounceBroadcast(w, { status: "active", suffix: pool, hops: [PUBLISHER_B] }, version));
		expect(await announced.next()).toMatchObject({ prefix: pool, kind: "start" });
	}

	const fresh = subscriber.consume(job);
	expect(fresh.closed).not.toBe(held.closed);
	expect(held.closed.peek()).toBeUndefined();

	fresh.close();
	held.close();
	announced.close();
	subscriber.close();
});

test("an update that re-prices the same publisher emits the new route", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("room"),
		kind: "start",
		route: { hops: [PUBLISHER_A, PEER], cost: 0n },
	});

	await send((w) =>
		encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [PUBLISHER_A], cost: 4n }, Version.DRAFT_06),
	);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("room"),
		kind: "update",
		route: { hops: [PUBLISHER_A, PEER], cost: 4n },
	});

	announced.close();
	subscriber.close();
});

test("a lite-05 duplicate announce follows the same update rule", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_05);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_05));
	const active = (hops: ReturnType<typeof HopSchema.parse>[]) => (w: Writer) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room"), hops }, Version.DRAFT_05);

	await send(active([PUBLISHER_A]));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	// On lite-05 an update travels as a duplicate ANNOUNCE rather than its own message.
	await send(active([PUBLISHER_A]));
	await send(active([PUBLISHER_B]));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "update" });

	announced.close();
	subscriber.close();
});

// A responder that withholds its Hop ID sends the reserved 0, and an empty chain means it
// originated the path itself, so the advertisement names nobody. An update of it is a reprice
// that updates in place and keeps the shared broadcast, as does one that names a publisher.
test("an update from an unidentified publisher updates in place", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();
	const room = Path.from("room");

	await send((w) => new AnnounceOk(UNKNOWN_HOP, 0).encode(w, Version.DRAFT_06));
	await send((w) => encodeAnnounceBroadcast(w, { status: "active", suffix: room, hops: [] }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "start" });
	const held = subscriber.consume(room);

	await send((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [], cost: 4n }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "update" });
	expect(subscriber.consume(room).closed).toBe(held.closed);

	// Naming a publisher changes the route, not the broadcast.
	await send((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "update" });
	expect(subscriber.consume(room).closed).toBe(held.closed);

	announced.close();
	subscriber.close();
});

// Encode PROBE messages the way a publisher would, so the subscriber's own loop
// decodes them.
async function probeBytes(probes: Probe[], version: Version): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
		version,
	);
	for (const probe of probes) await probe.encode(writer, version);
	writer.close();
	await writer.closed;

	const total = chunks.reduce((sum, c) => sum + c.byteLength, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const c of chunks) {
		out.set(c, offset);
		offset += c.byteLength;
	}
	return out;
}

/**
 * Drive `Subscriber.runProbe` over a canned script and return what the probe signal
 * held once the last message had been applied.
 *
 * Snapshotted before the stream is closed: `runProbe`'s `finally` blanks the signal
 * on exit, so reading afterwards would report `{}` no matter what the loop did. The
 * wait keys off the final message's bitrate, so give the script a distinct one.
 */
async function runProbeScript(version: Version, probes: Probe[], initial: ProbeStats = {}): Promise<ProbeStats> {
	let readableController!: ReadableStreamDefaultController<Uint8Array>;
	const quic = {
		createBidirectionalStream: async () => ({
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (readableController = controller) }),
			writable: new WritableStream<Uint8Array>(),
		}),
	} as unknown as WebTransport;

	const signal = new Signal<ProbeStats>(initial);
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n), signal);
	const running = subscriber.runProbe();

	await Promise.resolve();
	readableController.enqueue(await probeBytes(probes, version));

	const last = probes[probes.length - 1];
	while (signal.peek().estimatedRecvRate !== last.bitrate) await signal.changed();
	const snapshot = signal.peek();

	readableController.close();
	await running.catch(() => {});
	return snapshot;
}

// From lite-04 the RTT field is always on the wire and 0 explicitly means unknown, so
// an absent value is the publisher retracting a reading rather than declining to
// repeat it. Holding the old value would keep the jitter buffer adapting to a
// measurement the publisher had already withdrawn.
test("an RTT retraction clears the reading on lite-04+", async () => {
	const got = await runProbeScript(Version.DRAFT_05, [
		new Probe({ bitrate: 1_000_000, rtt: 40 }),
		new Probe({ bitrate: 2_000_000, rtt: undefined }),
	]);
	// The second bitrate proves the retracting message was applied, so an undefined
	// RTT here is the retraction landing rather than the loop never having run.
	expect(got.estimatedRecvRate).toBe(2_000_000);
	expect(got.rtt).toBeUndefined();
});

// lite-03's PROBE carries no RTT field at all, so an absent value there is "not
// carried" rather than a retraction, and a reading already on the signal must stand.
// Seeded rather than sent, because lite-03 has no way to put one on the wire.
test("lite-03 keeps an existing RTT, since its PROBE cannot carry one", async () => {
	const got = await runProbeScript(Version.DRAFT_03, [new Probe({ bitrate: 2_000_000 })], {
		rtt: Duration.millis(40),
	});
	expect(got.estimatedRecvRate).toBe(2_000_000);
	expect(got.rtt).toEqual(Duration.millis(40));
});

test("an announce skipped as a reflected loop still holds its path", async () => {
	// The subscriber's own origin, so a chain naming it reflects back through us.
	const SELF = 1n;
	const { subscriber, send, abortReason, sessionEnded, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));

	// Announce id 0: the chain loops back through us, so it is skipped locally. The peer
	// numbered it regardless and still holds the path.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [HopSchema.parse(SELF)] },
			Version.DRAFT_06,
		),
	);
	await settle();

	// A second start for that path is one advertisement too many, whether or not we made
	// anything of the first. Accepting it is what let id 0's end retract this one's state.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] },
			Version.DRAFT_06,
		),
	);

	await expect(announced.next()).rejects.toThrow("duplicate announce");
	// The peer has to hear about it: closing only our side would leave it announcing
	// into a stream nobody reads.
	expect(await abortReason()).toContain("duplicate announce");
	// The draft makes this session-fatal: resetting only the stream would let the peer
	// repeat the violation on the next one. The code has to say so too, or the peer reads
	// the default 0 as a clean close.
	expect((await sessionEnded())?.closeCode).toBe(SessionCode.ProtocolViolation);

	announced.close();
	subscriber.close();
});

test("an update replaces an announce that was skipped as a reflected loop", async () => {
	const SELF = 1n;
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));

	// Skipped: the chain reflects back through us.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [HopSchema.parse(SELF)] },
			Version.DRAFT_06,
		),
	);
	await settle();

	// The id stays live, so the peer may update it into a route that is usable here.
	await send((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	// Retiring the id ends what the update attached, and nothing else.
	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "end" });

	announced.close();
	subscriber.close();
});

test("retiring an id whose announce was skipped ends nothing", async () => {
	const SELF = 1n;
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));

	// Id 0 for "room": skipped as a reflected loop.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [HopSchema.parse(SELF)] },
			Version.DRAFT_06,
		),
	);
	// Id 1 for a different path, which is routable.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("lobby"), hops: [PUBLISHER_A] },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("lobby"), kind: "start" });

	// Retire the skipped one, then the live one. The first must surface nothing, so the
	// only end a consumer sees is "lobby".
	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_06));
	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 1n }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("lobby"), kind: "end" });

	announced.close();
	subscriber.close();
});

test("a draft-02 initial announcement can still be retracted", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_02);
	const announced = subscriber.routes();
	await settle();

	// ANNOUNCE_INIT carries the initial set. These are advertisements like any other, so
	// the peer may retract one later and the consumer has to hear about it.
	await send((w) => new AnnounceInit([Path.from("room")]).encode(w, Version.DRAFT_02));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	await send((w) => encodeAnnounceBroadcast(w, { status: "ended", suffix: Path.from("room") }, Version.DRAFT_02));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "end" });

	announced.close();
	subscriber.close();
});

test("a duplicate start is reported even when its own route reflects", async () => {
	const SELF = 1n;
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));

	// A first start we accept and surface.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	// A second start for that path, carrying a route that loops back through us. Skipping
	// it must not pre-empt the violation: the peer sent two starts with no end between
	// them regardless of whether the second one's route was usable.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [HopSchema.parse(SELF)] },
			Version.DRAFT_06,
		),
	);

	await expect(announced.next()).rejects.toThrow("duplicate announce");

	announced.close();
	subscriber.close();
});

test("a draft-02 initial set naming a path twice is refused", async () => {
	const { subscriber, send, abortReason, sessionEnded, settle } = announceHarness(Version.DRAFT_02);
	const announced = subscriber.routes();
	await settle();

	// One advertisement per path, and the initial set is advertisements. Two entries for
	// one path is the same violation as two ANNOUNCE_STARTs for it, which `start_announce`
	// already rejects on the Rust side.
	await send((w) => new AnnounceInit([Path.from("room"), Path.from("room")]).encode(w, Version.DRAFT_02));

	// Erroring the stream discards what it had already queued, so the consumer sees the
	// violation rather than the first entry followed by it.
	await expect(announced.next()).rejects.toThrow("duplicate announce");
	expect(await abortReason()).toContain("duplicate announce");
	// The draft makes this session-fatal: resetting only the stream would let the peer
	// repeat the violation on the next one. The code has to say so too, or the peer reads
	// the default 0 as a clean close.
	expect((await sessionEnded())?.closeCode).toBe(SessionCode.ProtocolViolation);

	announced.close();
	subscriber.close();
});

test("a violation on a very long path still ends the session", async () => {
	const SELF = 1n;
	const { subscriber, send, sessionEnded, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));

	// The path is peer-supplied and lands in the close reason. WebTransport throws on a
	// reason over 1024 bytes of UTF-8, and `#runAnnounced` is launched with `void`, so an
	// unbounded reason would surface as an unhandled rejection with the session still up.
	const long = Path.from("x".repeat(2000));
	const active: AnnounceBroadcast = { status: "active", suffix: long, hops: [PUBLISHER_A] };
	await send((w) => encodeAnnounceBroadcast(w, active, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: long, kind: "start" });

	await send((w) => encodeAnnounceBroadcast(w, active, Version.DRAFT_06));
	await expect(announced.next()).rejects.toThrow("duplicate announce");

	const info = await sessionEnded();
	expect(info?.closeCode).toBe(SessionCode.ProtocolViolation);
	expect(new TextEncoder().encode(info?.reason ?? "").byteLength).toBeLessThanOrEqual(1024);
});

test.each([Version.DRAFT_02, Version.DRAFT_06])(
	"version %p refuses a suffix that only the interest prefix pushes past the path limit",
	async (version) => {
		const { subscriber, send, sessionEnded, settle } = announceHarness(version);
		// Each half fits on the wire alone; the joined path is one byte over.
		const half = Path.from("x".repeat(Path.MAX_BYTES / 2));
		const announced = subscriber.routes(Path.Pattern.subtree(half));
		await settle();

		if (version === Version.DRAFT_02) {
			await send((w) => new AnnounceInit([half]).encode(w, version));
		} else {
			await send((w) => new AnnounceOk(PEER, 0).encode(w, version));
			const active: AnnounceBroadcast = { status: "active", suffix: half, hops: [PUBLISHER_A] };
			await send((w) => encodeAnnounceBroadcast(w, active, version));
		}

		await expect(announced.next()).rejects.toThrow(`path exceeds ${Path.MAX_BYTES} bytes`);
		expect((await sessionEnded())?.closeCode).toBe(SessionCode.ProtocolViolation);

		announced.close();
		subscriber.close();
	},
);

test("a draft-04 duplicate start is a violation, not an update", async () => {
	// Pre-lite-05 has no replacement idiom, so a second ANNOUNCE_START for a live path is
	// the same violation lite-06 treats it as. Only lite-05, where the duplicate *is* the
	// update, is exempt. The Rust loop routes every other version through `start_announce`,
	// which rejects it.
	const { subscriber, send, sessionEnded, settle } = announceHarness(Version.DRAFT_04);
	const announced = subscriber.routes();
	await settle();

	const active: AnnounceBroadcast = { status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] };
	await send((w) => encodeAnnounceBroadcast(w, active, Version.DRAFT_04));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	await send((w) => encodeAnnounceBroadcast(w, active, Version.DRAFT_04));

	// Session first: it is the bounded assertion, so a version that treats this as an
	// update fails here in 250ms instead of hanging on a rejection that never comes.
	expect((await sessionEnded())?.closeCode).toBe(SessionCode.ProtocolViolation);
	await expect(announced.next()).rejects.toThrow("duplicate announce");
});

test("a draft-05 duplicate start is still an update", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_05);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_05));

	const active: AnnounceBroadcast = { status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] };
	await send((w) => encodeAnnounceBroadcast(w, active, Version.DRAFT_05));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	// Same publisher over a new route: transparent, and emphatically not an error.
	await send((w) => encodeAnnounceBroadcast(w, active, Version.DRAFT_05));

	// A different publisher surfaces as an update, which is what proves the reroute above passed.
	const replaced: AnnounceBroadcast = { status: "active", suffix: Path.from("room"), hops: [PUBLISHER_B] };
	await send((w) => encodeAnnounceBroadcast(w, replaced, Version.DRAFT_05));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "update" });

	announced.close();
	subscriber.close();
});

// Mirrors the Rust `an_unknown_announce_type_keeps_the_stream`.
test("an unknown announce type between starts keeps the stream", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();

	const start = (suffix: string) => (w: Writer) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from(suffix), hops: [] }, Version.DRAFT_06);
	await send((w) => new AnnounceOk(PEER, 2).encode(w, Version.DRAFT_06));
	await send(start("a"));
	// An unknown announce type with an empty body, which decodes as skipped.
	await send((w) => w.write(new Uint8Array([0x3f, 0x00])));
	await send(start("b"));

	expect(await announced.next()).toMatchObject({ prefix: Path.from("a"), kind: "start" });
	expect(await announced.next()).toMatchObject({ prefix: Path.from("b"), kind: "start" });

	announced.close();
	subscriber.close();
});

interface FakeStream {
	inbound: ReadableStreamDefaultController<Uint8Array>;
	// The subscriber's writes as they arrive, so a decoder can wait for a whole message.
	outbound: ReadableStream<Uint8Array>;
	// Resolves once the subscriber waits on a read the test has not answered.
	reading: Promise<void>;
	aborted: Promise<unknown>;
	// Resolves once the subscriber FINs its side.
	finished: Promise<void>;
	// Resolves on the first write, even if it stalls.
	writing: Promise<void>;
	// Every chunk the subscriber wrote.
	written: Uint8Array[];
	// Hands the stream to the subscriber, for an open the session was told to park.
	release: () => void;
	// Fails every later write with `reason`, as a peer's STOP_SENDING does. Resolves on the first.
	stop: (reason: unknown) => Promise<void>;
}

// A session whose streams the test answers by hand and that never fails them on its own, so
// only Subscriber.close() can end a wait. Opens numbered in `park` wait for `release()`, and
// writes to those numbered in `stall` never get credit.
function fakeSession(park: number[] = [], stall: number[] = []) {
	const streams: FakeStream[] = [];
	const count = new Signal(0);
	async function opened(index: number): Promise<FakeStream> {
		while (streams.length <= index) await count.changed();
		return streams[index];
	}
	const closes: (WebTransportCloseInfo | undefined)[] = [];
	const quic = {
		close: (info?: WebTransportCloseInfo) => void closes.push(info),
		createBidirectionalStream: () => {
			let inbound!: ReadableStreamDefaultController<Uint8Array>;
			let sent!: ReadableStreamDefaultController<Uint8Array>;
			const outbound = new ReadableStream<Uint8Array>({ start: (controller) => (sent = controller) });
			let onRead!: () => void;
			let onAbort!: (reason: unknown) => void;
			let onFinish!: () => void;
			let onWrite!: () => void;
			let release!: () => void;
			let stopped: { reason: unknown; refused: () => void } | undefined;
			const reading = new Promise<void>((resolve) => (onRead = resolve));
			const aborted = new Promise<unknown>((resolve) => (onAbort = resolve));
			const finished = new Promise<void>((resolve) => (onFinish = resolve));
			const writing = new Promise<void>((resolve) => (onWrite = resolve));
			const stalled = stall.includes(streams.length);
			// No high water mark, so pull() means the subscriber is blocked on a read.
			const readable = new ReadableStream<Uint8Array>(
				{
					start: (controller) => {
						inbound = controller;
					},
					pull: () => onRead(),
				},
				{ highWaterMark: 0 },
			);
			const written: Uint8Array[] = [];
			const writable = new WritableStream<Uint8Array>({
				write: (chunk) => {
					if (stopped) {
						stopped.refused();
						return Promise.reject(stopped.reason);
					}
					const copy = new Uint8Array(chunk);
					written.push(copy);
					sent.enqueue(copy);
					onWrite();
					return new Promise<void>((resolve) => {
						if (stalled) return;
						// Transport credit can arrive in a task, outside the microtask queue.
						const channel = new MessageChannel();
						channel.port1.onmessage = () => {
							channel.port1.close();
							channel.port2.close();
							resolve();
						};
						channel.port2.postMessage(null);
					});
				},
				close: () => {
					sent.close();
					onFinish();
				},
				abort: (reason) => {
					sent.error(reason);
					onAbort(reason);
				},
			});
			const opened = new Promise((resolve) => (release = () => resolve({ readable, writable })));
			if (!park.includes(streams.length)) release();
			const stop = (reason: unknown) =>
				new Promise<void>((refused) => {
					stopped = { reason, refused };
				});
			streams.push({ inbound, outbound, reading, aborted, finished, writing, written, release, stop });
			count.set(streams.length);
			return opened;
		},
	} as unknown as WebTransport;
	return { quic, streams, opened, closes };
}

async function answerTrackInfo(
	stream: FakeStream,
	version: Version = Version.DRAFT_05,
	timescale?: number,
): Promise<void> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
		version,
	);
	await new TrackInfo({ timescale }).encode(writer, version);
	for (const chunk of chunks) stream.inbound.enqueue(chunk);
	stream.inbound.close();
}

function expectCut(err: unknown, cause: Error | undefined) {
	if (cause) {
		expect(err).toBe(cause);
	} else {
		// A deliberate close cuts it off as the peer's clean close would.
		expect(err).toBeInstanceOf(SessionError);
		expect((err as SessionError).code).toBe(SessionCode.Cancel);
	}
}

// Lite has no FETCH_OK, so a publisher that never answers holds each setup stage until the
// subscriber closes. The stream that stage opened is reset; one still waiting to open leaves
// the line, so the stream admitted for it later goes unused to the next open.
test.each([
	["the TRACK_INFO", "track", undefined],
	["the FETCH", "fetch", undefined],
	["the FETCH, on a session error", "fetch", new Error("session died")],
	["a stream slot for the FETCH", "open", undefined],
] as const)("closing the subscriber rejects a fetch waiting on %s", async (_, stage, cause) => {
	const { quic, streams, opened } = fakeSession(stage === "open" ? [1] : []);
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	let settled = false;
	const fetch = subscriber.fetchGroup(Path.from("room"), "video", 0n).then(
		() => {
			settled = true;
			return undefined;
		},
		(err: unknown) => {
			settled = true;
			return err;
		},
	);

	await opened(0);
	if (stage === "track") {
		await streams[0].reading;
	} else {
		await answerTrackInfo(streams[0]);
		await opened(1);
		if (stage === "fetch") await streams[1].reading;
	}
	expect(settled).toBe(false);

	subscriber.close(cause);
	expectCut(await fetch, cause);

	const stuck = streams[stage === "track" ? 0 : 1];
	stuck.release();
	if (stage === "open") {
		await drainReactions();
		expect(stuck.written).toEqual([]);
		return;
	}
	await stuck.aborted;
});

// An unanswered setup waits as long as it is wanted. Once its last subscriber leaves it is over: its
// TRACK stream is reset along with the SUBSCRIBE sent beside it,
// and the subscription is not registered again.
test.each([
	["lite-05 subscribe waiting on the TRACK_INFO", Version.DRAFT_05],
	["lite-06 subscribe waiting on the TRACK_INFO", Version.DRAFT_06],
	["lite-07 subscribe waiting on the TRACK_INFO", Version.DRAFT_07],
] as const)("a %s abandoned before its answer leaves nothing behind", async (_, version) => {
	const clock = new Manual();
	const installed = clock.install();
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	try {
		const track = subscriber.consume(Path.from("room")).track("video").subscribe();
		await opened(0);
		await streams[0].reading;

		await clock.advance(Duration.millis(3_600_000));
		expect(track.closed.peek()).toBeUndefined();
		track.close();

		await streams[0].aborted;
		await streams[1].aborted;
		expect(streams.length).toBe(2);

		// A GROUP for a forgotten id is ignored without touching its stream.
		const touched: PropertyKey[] = [];
		const reader = new Proxy({} as Reader, {
			get: (_, key) => {
				touched.push(key);
				return () => {};
			},
		});
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 0n }), reader);
		expect(touched).toEqual([]);
	} finally {
		subscriber.close();
		warn.mockRestore();
		installed.close();
	}
});

// Waiting for a stream slot is waiting its turn: the SUBSCRIBE goes out once credit returns,
// however long that takes.
test("a lite subscribe waiting on a stream slot for the SUBSCRIBE outlasts the deadline", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession([1]);
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	try {
		const track = subscriber.consume(Path.from("room")).track("video").subscribe();
		await opened(0);
		await streams[0].reading;
		await answerTrackInfo(streams[0]);
		await opened(1);

		await clock.advance(Duration.millis(3_600_000));
		expect(track.closed.peek()).toBeUndefined();

		streams[1].release();
		await streams[1].reading;
		expect(streams[1].written).not.toEqual([]);
	} finally {
		subscriber.close();
		warn.mockRestore();
		installed.close();
	}
});

// A SUBSCRIBE given up while it waits for a stream slot leaves the line: the stream admitted
// for it goes unused to the next open, rather than being reset.
test("a lite SUBSCRIBE open given up leaves its stream to the next open", async () => {
	const { quic, streams, opened } = fakeSession([1]);
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	try {
		// Closing the broadcast retires the setup while its SUBSCRIBE waits for a stream.
		const room = subscriber.consume(Path.from("room"));
		room.track("video").subscribe();
		await opened(0);
		await streams[0].reading;
		await answerTrackInfo(streams[0]);
		await opened(1);
		room.close();
		await drainReactions();
		streams[1].release();
		await drainReactions();
		expect(streams[1].written).toEqual([]);

		const other = subscriber.consume(Path.from("other"));
		other.track("video").subscribe();
		await streams[1].writing;
		other.close();
	} finally {
		subscriber.close();
		warn.mockRestore();
	}
});

// Leaving during SUBSCRIBE setup releases its TRACK bridge while the broadcast stays live.
test.each(["credit", "answer"] as const)("abandoning SUBSCRIBE during %s releases TRACK", async (stage) => {
	const { quic, streams, opened } = fakeSession(stage === "credit" ? [1] : []);
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const room = subscriber.consume(Path.from("room"));
	try {
		const video = room.track("video").subscribe();
		await opened(0);
		await streams[0].reading;
		await answerTrackInfo(streams[0], Version.DRAFT_07);
		await opened(1);
		if (stage === "answer") await streams[1].reading;
		video.close();
		await drainReactions();
		if (stage === "credit") await streams[0].aborted;
		else await streams[0].finished;
		if (stage === "credit") {
			streams[1].release();
			await drainReactions();
			expect(streams[1].written).toEqual([]);
		}
		const audio = room.track("audio").subscribe();
		await (await opened(stage === "credit" ? 1 : 2)).writing;
		audio.close();
	} finally {
		room.close();
		subscriber.close();
		warn.mockRestore();
	}
});

// The last reader leaving gives the setup up while its TRACK stream still waits for a slot, so
// the stream admitted for it goes unused to the next open.
test("a lite subscribe whose readers leave while it waits for a stream gives up the wait", async () => {
	const { quic, streams, opened } = fakeSession([0]);
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const room = subscriber.consume(Path.from("room"));
	try {
		const video = room.track("video").subscribe();
		await opened(0);
		video.close();
		await drainReactions();
		streams[0].release();
		await drainReactions();
		expect(streams[0].written).toEqual([]);

		const audio = room.track("audio").subscribe();
		await streams[0].writing;
		audio.close();
	} finally {
		room.close();
		subscriber.close();
		warn.mockRestore();
	}
});

// A SUBSCRIBE write blocked on flow control may never let the setup settle, so abandonment
// lets go of the held TRACK stream without waiting for it.
test("a lite subscribe abandoned while its SUBSCRIBE write stalls closes its TRACK stream", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession([], [1]);
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	try {
		const track = subscriber.consume(Path.from("room")).track("video").subscribe();
		await opened(0);
		await streams[0].reading;
		await answerTrackInfo(streams[0]);
		await (await opened(1)).writing;

		await clock.advance(Duration.millis(3_600_000));
		expect(track.closed.peek()).toBeUndefined();
		track.close();
		await streams[0].finished;
	} finally {
		subscriber.close();
		warn.mockRestore();
		installed.close();
	}
});

// On lite-05 and later a SUBSCRIBE goes out beside its TRACK, before the TRACK_INFO, and accepting
// the info sends no SUBSCRIBE_UPDATE even when the publisher keeps groups fresh for less than the
// subscriber waits.
test.each(
	[Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07].flatMap((version) =>
		[false, true].map((forced) => ({ name: versionName(version), version, forced })),
	),
)("$name: a subscribe goes out with its track (forced yields: $forced)", async ({ version, forced }) => {
	const yielding = forced ? spyOn(Budget.prototype, "poll").mockImplementation(() => yieldNow()) : undefined;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe({ priority: 3, delay: { max: Duration.millis(10_000) } });
	try {
		await opened(0);
		await streams[0].reading;
		const sent = new Reader(undefined, new Uint8Array(Buffer.concat(streams[0].written)), version);
		expect(await sent.u53()).toBe(StreamId.Track);
		const request = await opened(1);
		const subscribe = new Reader(request.outbound, undefined, version);
		expect(await subscribe.u53()).toBe(StreamId.Subscribe);
		// Reading the request waits for its body without needing TRACK_INFO first.
		const msg = await Subscribe.decode(subscribe, version);
		expect([msg.id, msg.track, msg.priority, msg.maxDelay]).toEqual([0n, "video", 3, 10_000]);

		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);
		await new TrackInfo({ timescale: 1000, maxAge: 1000 }).encode(writer, version);
		for (const chunk of chunks) streams[0].inbound.enqueue(chunk);
		await track.info();
		await request.reading;
		// An explicit change orders after any update caused by TRACK_INFO, even if its encoding yields.
		track.update({ priority: 4, delay: { max: Duration.millis(10_000) } });
		const update = await SubscribeUpdate.decode(subscribe, version);
		expect([update.priority, update.maxDelay]).toEqual([4, 10_000]);
		track.close();
		expect(await subscribe.done()).toBe(true);
	} finally {
		track.close();
		subscriber.close();
		yielding?.mockRestore();
	}
});

// A GROUP that beats its subscription's TRACK_INFO waits unread for it, then is read with it.
test("a group that beats its track info waits for it", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	try {
		await opened(0);
		await (await opened(1)).writing;
		const touched: PropertyKey[] = [];
		const stream = await groupStream(0, version);
		const reader = new Proxy(stream, {
			get: (target, key) => {
				touched.push(key);
				const value = Reflect.get(target, key, target);
				return typeof value === "function" ? value.bind(target) : value;
			},
		});
		const receiving = subscriber.runGroup(new Group({ subscribe: 0n, sequence: 0n }), reader);
		await drainReactions();
		expect(touched).toEqual([]);

		await answerTrackInfo(streams[0], version, 1000);
		await receiving;
		const group = await track.recvGroup();
		expect(group?.sequence).toBe(0n);
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([0]));
	} finally {
		track.close();
		subscriber.close();
	}
});

// A lite-07 FRAME: the payload size plus one, the zigzag timestamp delta, then the first `sent`
// bytes of the payload.
function liteFrame(delta: number, payload: Uint8Array, sent = payload.byteLength): Promise<Uint8Array> {
	return Writer.collect(Version.DRAFT_07, async (w) => {
		await w.u53(payload.byteLength + 1);
		await w.u62(BigInt(delta) * 2n);
		if (sent > 0) await w.write(payload.subarray(0, sent));
	});
}

// A group stream the test feeds by hand, and resets with a code of its choosing.
function handStream(version: Version) {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const readable = new ReadableStream<Uint8Array>({
		start: (c) => {
			controller = c;
		},
	});
	return {
		reader: new Reader(readable, undefined, version),
		write: (chunk: Uint8Array) => controller.enqueue(chunk),
		finish: () => controller.close(),
		reset: (code: StreamCode) => controller.error(toTransport(code, "reset")),
	};
}

// Lite-07: a stream reset with YIELDED pauses its group, keeping the frame it cut short, and a later
// stream continues it from the paused stream's Frame Start, whether it lands after that reset or
// overtakes it. The group's reader gets every frame once, then its end. Mirrors the Rust
// `a_yielded_group_continues_without_loss`.
test.each([false, true])("a yielded group continues without loss (overtakes: %p)", async (overtakes) => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe({ delay: { max: Duration.millis(60_000) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;

		// Frame 1 streams in past one segment, so the reset cuts it short partway.
		const big = new Uint8Array(SEGMENT_BYTES + 7).map((_, i) => i % 251);
		const frames = [
			{ delta: 300, payload: new TextEncoder().encode("zero") },
			{ delta: 10, payload: big },
			{ delta: 10, payload: new TextEncoder().encode("three") },
		];
		const first = handStream(version);
		first.write(await liteFrame(frames[0].delta, frames[0].payload));
		first.write(await liteFrame(frames[1].delta, frames[1].payload, SEGMENT_BYTES / 2));
		const second = handStream(version);
		for (const { delta, payload } of frames) second.write(await liteFrame(delta, payload));
		second.finish();

		const header = new Group({ subscribe: 0n, sequence: 3n, frameStart: 0 });
		const yielded = subscriber.runGroup(header, first.reader);
		const group = await track.recvGroup();
		expect(group?.sequence).toBe(3n);
		expect((await group?.readFrame())?.payload).toEqual(frames[0].payload);

		let continued: Promise<void> | undefined;
		if (overtakes) {
			let held = true;
			continued = subscriber.runGroup(header, second.reader).finally(() => {
				held = false;
			});
			await drainReactions();
			expect(held).toBe(true);
		}
		first.reset(StreamCode.Yielded);
		await yielded;
		await drainReactions();
		expect(group?.closed.peek()).toBeUndefined();
		continued ??= subscriber.runGroup(header, second.reader);
		await continued;

		for (const { payload } of frames.slice(1)) {
			const frame = await group?.readFrame();
			expect(frame?.payload.byteLength).toBe(payload.byteLength);
			expect(Buffer.compare(frame?.payload ?? new Uint8Array(), payload)).toBe(0);
		}
		expect(await group?.readFrame()).toBeUndefined();
		expect(await group?.closed).toBeNull();
	} finally {
		track.close();
		subscriber.close();
	}
});

// Lite-07: a continuation is reset with YIELDED too, pausing the group again, and a third stream
// finishes it. Each pause keeps what arrived, and the reader gets every frame once.
test("a group yielded twice continues without loss", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe({ delay: { max: Duration.millis(60_000) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;

		const frames = ["zero", "one", "two"].map((text, i) => ({
			delta: i === 0 ? 300 : 10,
			payload: new TextEncoder().encode(text),
		}));
		const header = new Group({ subscribe: 0n, sequence: 3n, frameStart: 0 });
		const group = (async () => track.recvGroup())();

		// Each stream starts at frame 0 and carries one frame more than the last before its reset.
		for (const sent of [1, 2]) {
			const stream = handStream(version);
			for (const { delta, payload } of frames.slice(0, sent)) stream.write(await liteFrame(delta, payload));
			const running = subscriber.runGroup(header, stream.reader);
			await drainReactions();
			stream.reset(StreamCode.Yielded);
			await running;
			await drainReactions();
			expect((await group)?.closed.peek(), `after ${sent} frames`).toBeUndefined();
		}
		const last = handStream(version);
		for (const { delta, payload } of frames) last.write(await liteFrame(delta, payload));
		last.finish();
		await subscriber.runGroup(header, last.reader);

		const reader = await group;
		expect(reader?.sequence).toBe(3n);
		for (const { payload } of frames) expect((await reader?.readFrame())?.payload).toEqual(payload);
		expect(await reader?.readFrame()).toBeUndefined();
		expect(await reader?.closed).toBeNull();
	} finally {
		track.close();
		subscriber.close();
	}
});

// Lite-07: a drop can overtake the YIELDED reset of the stream it names. The group ends with the
// drop's error rather than waiting for a continuation the publisher never sends. Mirrors the Rust
// `a_yielded_group_a_drop_named_ends`.
test("a yielded group a drop named ends", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe({ delay: { max: Duration.millis(60_000) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		const request = await opened(1);
		await request.reading;
		await respond(request, version, [{ start: new SubscribeStart([0n]) }]);

		const first = handStream(version);
		first.write(await liteFrame(300, new Uint8Array([1])));
		first.write(await liteFrame(10, new Uint8Array([2, 3]), 1));
		const yielded = subscriber.runGroup(new Group({ subscribe: 0n, sequence: 3n }), first.reader);
		const group = await track.recvGroup();
		expect(group?.sequence).toBe(3n);

		// The publisher gave the group up after pausing it, and its drop lands first.
		await respond(request, version, [{ drop: new SubscribeDrop({ start: 3n, end: 4n, error: StreamCode.Old }) }]);
		await drainReactions();
		first.reset(StreamCode.Yielded);
		await yielded;

		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([1]));
		const read = await group?.readFrame().then(
			() => undefined,
			(err: unknown) => err,
		);
		expect(read).toBeInstanceOf(StreamError);
		expect((read as StreamError).code).toBe(StreamCode.Old);
	} finally {
		track.close();
		subscriber.close();
	}
});

// Lite-07: a stale drop that overtook the YIELDED reset of the stream it names holds only under the
// update it names: once the subscriber sends another, the reset pauses the group, which the
// publisher then continues or drops again. Mirrors the Rust
// `a_yielded_group_pauses_once_an_update_supersedes_its_stale_drop`.
test("a yielded group pauses once an update supersedes its stale drop", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe({ delay: { max: Duration.millis(60_000) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		const request = await opened(1);
		await request.reading;
		await respond(request, version, [{ start: new SubscribeStart([0n]) }]);

		const first = handStream(version);
		first.write(await liteFrame(300, new Uint8Array([1])));
		const yielded = subscriber.runGroup(new Group({ subscribe: 0n, sequence: 3n }), first.reader);
		const group = await track.recvGroup();
		expect(group?.sequence).toBe(3n);
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([1]));

		await respond(request, version, [{ drop: new SubscribeDrop({ start: 3n, end: 4n, error: StreamCode.Old }) }]);
		await drainReactions();
		track.update({ priority: 1, delay: { max: Duration.millis(60_000) } });
		await drainReactions();
		first.reset(StreamCode.Yielded);
		await yielded;
		await drainReactions();

		expect(group?.closed.peek()).toBeUndefined();
		// The continuation picks the group up where the reset left it.
		const second = handStream(version);
		second.write(await liteFrame(10, new Uint8Array([2])));
		second.finish();
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 3n, frameStart: 1 }), second.reader);
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([2]));
		expect(await group?.readFrame()).toBeUndefined();
	} finally {
		track.close();
		subscriber.close();
	}
});

// Lite-07: a stale drop that overtook the header of the stream it names holds only under the update
// it names: once the subscriber sends another, a YIELDED reset of that stream pauses the group.
// Mirrors the Rust `a_yielded_group_pauses_once_an_update_supersedes_a_stale_drop_before_its_header`.
test("a yielded group pauses once an update supersedes a stale drop before its header", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe({ delay: { max: Duration.millis(60_000) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		const request = await opened(1);
		await request.reading;
		await respond(request, version, [{ start: new SubscribeStart([0n]) }]);

		await respond(request, version, [{ drop: new SubscribeDrop({ start: 3n, end: 4n, error: StreamCode.Old }) }]);
		await drainReactions();
		const first = handStream(version);
		first.write(await liteFrame(300, new Uint8Array([1])));
		const yielded = subscriber.runGroup(new Group({ subscribe: 0n, sequence: 3n }), first.reader);
		const group = await track.recvGroup();
		expect(group?.sequence).toBe(3n);
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([1]));

		track.update({ priority: 1, delay: { max: Duration.millis(60_000) } });
		await drainReactions();
		first.reset(StreamCode.Yielded);
		await yielded;
		await drainReactions();

		expect(group?.closed.peek()).toBeUndefined();
		// The continuation picks the group up where the reset left it.
		const second = handStream(version);
		second.write(await liteFrame(10, new Uint8Array([2])));
		second.finish();
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 3n, frameStart: 1 }), second.reader);
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([2]));
		expect(await group?.readFrame()).toBeUndefined();
	} finally {
		track.close();
		subscriber.close();
	}
});

// Lite-07: a malformed FRAME on a Group Stream ends only that stream and its group, never the
// session: a Payload Length of 0, or a continuation that resends the cut frame at another size, ends
// before completing it, or starts past the frames held. Mirrors the Rust `UniState::Group`.
test.each(["zero length", "resized", "unfinished", "past held"] as const)(
	"a malformed lite-07 group stream ends only its group (%s)",
	async (fault) => {
		const version = Version.DRAFT_07;
		const { quic, streams, opened } = fakeSession();
		const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
		const track = subscriber
			.consume(Path.from("room"))
			.track("video")
			.subscribe({ delay: { max: Duration.millis(60_000) } });
		try {
			await opened(0);
			await answerTrackInfo(streams[0], version, 1000);
			await (await opened(1)).reading;

			const zero = new TextEncoder().encode("zero");
			// Past one segment, so the reset cuts it short partway.
			const cut = new Uint8Array(SEGMENT_BYTES + 7);
			const header = new Group({ subscribe: 0n, sequence: 3n, frameStart: 0 });
			const first = handStream(version);
			first.write(await liteFrame(300, zero));
			if (fault === "zero length") first.write(await Writer.collect(version, (w) => w.u53(0)));
			else first.write(await liteFrame(10, cut, SEGMENT_BYTES / 2));
			let running = subscriber.runGroup(header, first.reader);
			const group = await track.recvGroup();
			expect((await group?.readFrame())?.payload).toEqual(zero);

			if (fault !== "zero length") {
				first.reset(StreamCode.Yielded);
				await running;
				await drainReactions();
				expect(group?.closed.peek()).toBeUndefined();
				const second = handStream(version);
				if (fault === "past held") {
					// Only frame 0 is held whole, so frame 2 would leave a hole.
					running = subscriber.runGroup(
						new Group({ subscribe: 0n, sequence: 3n, frameStart: 2 }),
						second.reader,
					);
				} else {
					second.write(await liteFrame(300, zero));
					if (fault === "resized") second.write(await liteFrame(10, new Uint8Array(cut.byteLength + 1)));
					running = subscriber.runGroup(header, second.reader);
				}
				second.finish();
			}

			// The stream ends without the session: nothing escapes for the connection to close it with.
			await expect(running).resolves.toBeUndefined();
			expect(await group?.closed).toMatchObject({ code: StreamCode.MalformedTrack });
		} finally {
			track.close();
			subscriber.close();
		}
	},
);

// Before lite-07 a YIELDED reset is no pause: the group ends with it, as with any other reset.
test("a lite-06 group reset with YIELDED ends", async () => {
	const version = Version.DRAFT_06;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;
		const first = handStream(version);
		const frame = await Writer.collect(version, async (w) => {
			await w.u62(600n);
			await w.u53(1);
			await w.write(new Uint8Array([1]));
		});
		first.write(frame);
		const yielded = subscriber.runGroup(new Group({ subscribe: 0n, sequence: 0n }), first.reader);
		const group = await track.recvGroup();
		first.reset(StreamCode.Yielded);
		await yielded;
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([1]));
		expect(await group?.closed).toMatchObject({ code: StreamCode.Yielded });
	} finally {
		track.close();
		subscriber.close();
	}
});

// A lite-07 re-offer can overtake the reset of the stream it replaces: it waits, unread, until that
// copy ends. It is refused as a duplicate once the copy finishes, and takes the group once it fails,
// continuing the reads of that copy from their frame.
test.each([false, true])("a duplicate of an arriving copy waits for it to settle (first fails: %p)", async (fails) => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	try {
		await opened(0);
		await (await opened(1)).writing;
		await answerTrackInfo(streams[0], version, 1000);

		// The original copy, still arriving: one frame in, no end yet.
		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);
		// A lite-07 FRAME: the size plus one, a zero timestamp delta, then the payload.
		await writer.u53(2);
		await writer.u62(0n);
		await writer.write(new Uint8Array([7]));
		let first!: ReadableStreamDefaultController<Uint8Array>;
		const arriving = new ReadableStream<Uint8Array>({
			start: (controller) => {
				first = controller;
			},
		});
		const original = subscriber.runGroup(
			new Group({ subscribe: 0n, sequence: 0n }),
			new Reader(arriving, undefined, version),
		);
		for (const chunk of chunks) first.enqueue(chunk);
		const group = await track.recvGroup();
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([7]));

		const resent: Uint8Array[] = [];
		const resend = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void resent.push(new Uint8Array(chunk)) }),
			version,
		);
		for (const [timestamp, payload] of [
			[0n, 7],
			[2n, 8],
		] as const) {
			// The size plus one first, as above.
			await resend.u53(2);
			await resend.u62(timestamp);
			await resend.write(new Uint8Array([payload]));
		}
		const duplicate = new Reader(
			new ReadableStream({
				start: (controller) => {
					for (const chunk of resent) controller.enqueue(chunk);
					controller.close();
				},
			}),
			undefined,
			version,
		);
		const stop = spyOn(duplicate, "stop");
		let held = true;
		const receiving = subscriber.runGroup(new Group({ subscribe: 0n, sequence: 0n }), duplicate).finally(() => {
			held = false;
		});
		await drainReactions();
		expect(held).toBe(true);
		expect(stop).not.toHaveBeenCalled();

		if (!fails) {
			first.close();
			await original;
			await receiving;
			expect(stop.mock.calls[0]?.[0]).toBeInstanceOf(Error);
			expect(await group?.readFrame()).toBeUndefined();
			return;
		}

		// The reset lands: the reader already handed the original waits on the held stream rather
		// than failing, and continues on it from its frame once it takes the group.
		const next = group?.readFrame();
		first.error(new Error("peer reset"));
		await original;
		await receiving;
		expect((await next)?.payload).toEqual(new Uint8Array([8]));
		expect(await group?.readFrame()).toBeUndefined();
		expect(await group?.closed).toBeNull();
		// One that had not reached it reads the re-offer whole.
		const later = subscriber.consume(Path.from("room")).track("video").subscribe();
		try {
			const again = await later.recvGroup();
			expect(again?.sequence).toBe(0n);
			expect((await again?.readFrame())?.payload).toEqual(new Uint8Array([7]));
			expect((await again?.readFrame())?.payload).toEqual(new Uint8Array([8]));
		} finally {
			later.close();
		}
	} finally {
		track.close();
		subscriber.close();
	}
});

// A TRACK reset while its SUBSCRIBE is live fails the subscription, so the origin can try another
// route, and resets the SUBSCRIBE.
test("a reset track rejects the subscription and resets its subscribe", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	try {
		await opened(0);
		await (await opened(1)).writing;
		streams[0].inbound.error(new Error("peer reset"));
		expect(await track.closed).toBeInstanceOf(Error);
		await streams[1].aborted;
	} finally {
		track.close();
		subscriber.close();
		warn.mockRestore();
	}
});

test("a fetch started after the subscriber closes rejects without opening a stream", async () => {
	const { quic, streams } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	subscriber.close();

	const err = await subscriber.fetchGroup(Path.from("room"), "video", 0n).catch((err: unknown) => err);
	expectCut(err, undefined);
	expect(streams.length).toBe(0);
});

// The grant watch is armed before the first check, so a shrink while TRACK_INFO is still in
// flight refuses the subscription and resets the SUBSCRIBE sent beside its TRACK.
test("a grant that shrinks while the subscription sets up refuses it", async () => {
	const { quic, streams, opened } = fakeSession();
	const scoped = (prefix: string): Grant => ({
		publish: new Path.Patterns([]),
		subscribe: new Path.Patterns([Path.Pattern.subtree(prefix)]),
	});
	const grant = new Signal<Grant | undefined>(scoped("room"));
	const subscriber = new Subscriber(
		quic,
		Version.DRAFT_05,
		HopSchema.parse(1n),
		undefined,
		undefined,
		undefined,
		grant,
	);

	const track = subscriber.consume(Path.from("room/cam")).track("video").subscribe().ordered();
	const next = track.nextGroup().catch((err: unknown) => err);

	// Parked on TRACK_INFO when the grant shrinks, which ends the TRACK exchange too.
	await opened(0);
	await streams[0].reading;
	grant.set(scoped("other"));

	const err = await next;
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.Unauthorized);
	await streams[1].aborted;
	expect(streams.length).toBe(2);

	track.close();
});

test("an already-aborted fetch rejects without opening a stream", async () => {
	const { quic, streams } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	const cause = new Error("gone");

	const err = await subscriber
		.fetchGroup(Path.from("room"), "video", 0n, { signal: AbortSignal.abort(cause) })
		.catch((err: unknown) => err);
	expect(err).toBe(cause);
	expect(streams.length).toBe(0);
});

// A fetch-only reader's FETCH goes out alongside the TRACK stream, not a round trip after it,
// and its response is read only once TRACK_INFO says the timescale.
test("the first FETCH goes out before TRACK_INFO answers", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	let fetched: GroupConsumer | undefined;
	const fetch = subscriber.fetchGroup(Path.from("room"), "video", 0n).then((group) => {
		fetched = group;
		return group;
	});
	await opened(0);
	await opened(1);
	await streams[1].reading;

	// An empty-group FIN accepts the fetch, but nothing is handed out before the info.
	streams[1].inbound.close();
	for (let i = 0; i < 64; i++) await Promise.resolve();
	expect(fetched).toBeUndefined();

	await answerTrackInfo(streams[0]);
	expect(await (await fetch).readFrame()).toBeUndefined();

	subscriber.close();
});

test("a failed TRACK_INFO fails the FETCH sent alongside it", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	const fetch = subscriber.fetchGroup(Path.from("room"), "video", 0n).catch((err: unknown) => err);
	await opened(0);
	await opened(1);
	await streams[1].reading;

	streams[0].inbound.error(new StreamError(StreamCode.NotFound, { message: "no such track" }));
	expect(await fetch).toBeInstanceOf(Error);
	await streams[1].aborted;

	subscriber.close();
});

// Coalesced fetches share one FETCH stream. An abort releases only that caller's share; the
// stream is cancelled once the last sharer leaves, before its FETCH is sent if it can be.
test("one of two fetch sharers aborting leaves the other's fetch", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	const controller = new AbortController();
	const a = subscriber.fetchGroup(Path.from("room"), "video", 0n, { signal: controller.signal });
	const b = subscriber.fetchGroup(Path.from("room"), "video", 0n);

	await opened(0);
	await answerTrackInfo(streams[0]);
	await opened(1);
	await streams[1].reading;

	const cause = new Error("gone");
	controller.abort(cause);
	expect(await a.catch((err: unknown) => err)).toBe(cause);

	let aborted = false;
	void streams[1].aborted.then(() => {
		aborted = true;
	});
	// An empty-group FIN accepts the fetch.
	streams[1].inbound.close();
	const group = await b;
	expect(await group.readFrame()).toBeUndefined();
	expect(aborted).toBe(false);

	subscriber.close();
});

test.each([
	["the TRACK_INFO", "track"],
	["the FETCH", "fetch"],
] as const)("the last fetch sharer aborting during %s cancels it", async (_, stage) => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	const first = new AbortController();
	const second = new AbortController();
	const a = subscriber.fetchGroup(Path.from("room"), "video", 0n, { signal: first.signal });
	const b = subscriber.fetchGroup(Path.from("room"), "video", 0n, { signal: second.signal });

	await opened(0);
	await streams[0].reading;
	if (stage === "fetch") {
		await answerTrackInfo(streams[0]);
		await opened(1);
		await streams[1].reading;
	}

	first.abort(new Error("first"));
	second.abort(new Error("second"));
	expect(((await a.catch((err: unknown) => err)) as Error).message).toBe("first");
	expect(((await b.catch((err: unknown) => err)) as Error).message).toBe("second");

	if (stage === "track") {
		// The abandoned TRACK exchange is reset promptly, with the FETCH sent alongside it.
		for (const stream of streams) {
			const err = fromTransport(await stream.aborted) as StreamError;
			expect(err.code).toBe(StreamCode.Cancel);
		}
		expect(streams.length).toBe(2);
	} else {
		const err = fromTransport(await streams[1].aborted) as StreamError;
		expect(err.code).toBe(StreamCode.Cancel);
	}

	subscriber.close();
});

// The fetch is withdrawn the moment it is cancelled, so a fetch arriving at any point after the
// last sharer left either revives it or starts a fresh one, never joins the cancelled one.
test("a fetch after the last sharer left is never failed with the cancelled one", async () => {
	for (let depth = 0; depth < 32; depth++) {
		const { quic, streams, opened } = fakeSession();
		const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

		const controller = new AbortController();
		const a = subscriber.fetchGroup(Path.from("room"), "video", 0n, { signal: controller.signal });
		void a.catch(() => undefined);
		await opened(0);
		await answerTrackInfo(streams[0]);
		await opened(1);
		await streams[1].reading;

		controller.abort(new Error("gone"));
		for (let i = 0; i < depth; i++) await Promise.resolve();
		const b = subscriber.fetchGroup(Path.from("room"), "video", 0n).catch((err: unknown) => err);
		for (let i = 0; i < 64; i++) await Promise.resolve();

		// An empty-group FIN accepts whichever FETCH the late caller is waiting on.
		if (streams.length > 2) {
			await answerTrackInfo(streams[2]);
			await opened(3);
			await streams[3].reading;
			streams[3].inbound.close();
		} else {
			// Only open if the late caller revived it.
			try {
				streams[1].inbound.close();
			} catch {}
		}
		const res = await b;
		expect(res instanceof Error ? `depth ${depth}: ${res.message}` : undefined).toBeUndefined();

		subscriber.close();
	}
});

// A fetch belongs to the consume that opened it. Once that consume is gone (a replaced
// instance's is evicted), a fresh one for the path opens its own FETCH rather than reading the
// old one's, even while the old FETCH is still open.
test("a fresh consume never joins a fetch an earlier one opened", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	const room = Path.from("room");

	const old = subscriber.consume(room);
	const fetch = old.track("video").fetchGroup(0n);
	await opened(0);
	await answerTrackInfo(streams[0]);
	await opened(1);
	await streams[1].reading;
	streams[1].inbound.enqueue(new Uint8Array([0, 4, ...new TextEncoder().encode("head")]));
	const group = await fetch;
	old.close();

	const fresh = subscriber.consume(room);
	expect(fresh.closed).not.toBe(old.closed);
	void fresh
		.track("video")
		.fetchGroup(0n)
		.catch(() => {});
	await opened(2);

	group.close();
	fresh.close();
	subscriber.close();
});

// A reader leaving partway through a fetched group cancels the FETCH: the truncated group
// must not end clean, as a FIN would make it read whole.
test("the last reader leaving mid-response cancels the fetch", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	const fetch = subscriber.fetchGroup(Path.from("room"), "video", 0n);
	await opened(0);
	await answerTrackInfo(streams[0]);
	await opened(1);
	await streams[1].reading;

	// One frame of the group: a zero timestamp delta, then the sized payload.
	streams[1].inbound.enqueue(new Uint8Array([0, 4, ...new TextEncoder().encode("head")]));
	const group = await fetch;
	expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("head");
	group.close();

	const err = fromTransport(await streams[1].aborted) as StreamError;
	expect(err.code).toBe(StreamCode.Cancel);

	subscriber.close();
});

test.each(["initial", "track", "fetch", "body"] as const)(
	"a revoked grant refuses a fetch during %s",
	async (stage) => {
		const { quic, streams, opened } = fakeSession();
		const scoped = (prefix: string): Grant => ({
			publish: new Path.Patterns(),
			subscribe: new Path.Patterns([Path.Pattern.subtree(prefix)]),
		});
		const grant = new Signal<Grant | undefined>(scoped(stage === "initial" ? "other" : "room"));
		const subscriber = new Subscriber(
			quic,
			Version.DRAFT_05,
			HopSchema.parse(1n),
			undefined,
			undefined,
			undefined,
			grant,
		);
		let result: unknown;
		const pending = subscriber.fetchGroup(Path.from("room/cam"), "video", 0n).then(
			(group) => {
				result = group;
				return group;
			},
			(err: unknown) => {
				result = err;
				return err;
			},
		);
		try {
			if (stage !== "initial") {
				await opened(0);
				await streams[0].reading;
				if (stage !== "track") {
					await answerTrackInfo(streams[0]);
					await opened(1);
					await streams[1].reading;
				}
				if (stage === "body") {
					streams[1].inbound.enqueue(new Uint8Array([0, 4, ...new TextEncoder().encode("head")]));
					await pending;
				}
				grant.set(scoped("other"));
			}
			if (stage === "body") {
				const group = result as Awaited<ReturnType<Subscriber["fetchGroup"]>>;
				await group.closed;
				expect(group.closed.peek()).toBeInstanceOf(StreamError);
				expect((group.closed.peek() as StreamError).code).toBe(StreamCode.Unauthorized);
				const reset = fromTransport(await streams[1].aborted) as StreamError;
				expect(reset.code).toBe(StreamCode.Unauthorized);
				group.close();
			} else {
				await pending;
				expect(result).toBeInstanceOf(StreamError);
				expect((result as StreamError).code).toBe(StreamCode.Unauthorized);
				if (stage === "initial") expect(streams).toHaveLength(0);
			}
		} finally {
			subscriber.close();
			await pending;
		}
	},
);

// Serve announcements and metadata on separate mock streams, with no wall-clock wait.
test.each([Version.DRAFT_05, Version.DRAFT_06])(
	"a retired handle on a wire without instances refuses new metadata on %s",
	async (version) => {
		const { quic, streams, opened } = fakeSession();
		const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
		const announced = subscriber.routes();
		await opened(0);
		const send = async (encode: (writer: Writer) => Promise<void>) => {
			const writer = new Writer(
				new WritableStream<Uint8Array>({
					write: (chunk) => streams[0].inbound.enqueue(new Uint8Array(chunk)),
				}),
				version,
			);
			await encode(writer);
		};
		const path = Path.from("pool");
		await send((writer) => new AnnounceOk(PEER, 0).encode(writer, version));
		await send((writer) =>
			encodeAnnounceBroadcast(writer, { status: "active", suffix: path, hops: [PUBLISHER_A] }, version),
		);
		await announced.next();
		const held = subscriber.consume(Path.from("pool/job"));
		const track = held.track("video");
		await send((writer) =>
			encodeAnnounceBroadcast(
				writer,
				version === Version.DRAFT_05 ? { status: "ended", suffix: path } : { status: "endedId", id: 0n },
				version,
			),
		);
		await announced.next();
		await send((writer) =>
			encodeAnnounceBroadcast(writer, { status: "active", suffix: path, hops: [PUBLISHER_B] }, version),
		);
		await announced.next();
		const result = track.info().catch((err: unknown) => err);
		await Promise.race([result, opened(1)]);
		// The unfixed implementation asks B and accepts its metadata.
		if (streams.length === 2) await answerTrackInfo(streams[1], version);
		expect(await result).toBeInstanceOf(Error);
		expect(streams.length).toBe(1);
		expect(held.closed.peek()).toBeUndefined();
		held.close();
		announced.close();
		subscriber.close();
	},
);

/** The instance an announcement names on a wire that carries one, `name` on lite-07. */
function named(version: Version, name: string): Instance.Valid | undefined {
	return version === Version.DRAFT_07 ? Instance.parse(name) : undefined;
}

// Announce PUBLISHER_A's `pool` on stream 0, under instance `a` where the wire carries one, and
// return how to withdraw it and how to announce PUBLISHER_B's, under `b`, in its place.
async function unpinnedPool(version: Version, streams: FakeStream[], announced: { next(): Promise<unknown> }) {
	const send = async (encode: (writer: Writer) => Promise<void>) => {
		const writer = new Writer(
			new WritableStream<Uint8Array>({
				write: (chunk) => streams[0].inbound.enqueue(new Uint8Array(chunk)),
			}),
			version,
		);
		await encode(writer);
	};
	const path = Path.from("pool");
	await send((writer) => new AnnounceOk(PEER, 0).encode(writer, version));
	await send((writer) =>
		encodeAnnounceBroadcast(
			writer,
			{ status: "active", suffix: path, hops: [PUBLISHER_A], instance: named(version, "a") },
			version,
		),
	);
	await announced.next();
	const withdraw = async () => {
		await send((writer) =>
			encodeAnnounceBroadcast(
				writer,
				version === Version.DRAFT_05 ? { status: "ended", suffix: path } : { status: "endedId", id: 0n },
				version,
			),
		);
		await announced.next();
	};
	const announce = async () => {
		await send((writer) =>
			encodeAnnounceBroadcast(
				writer,
				{ status: "active", suffix: path, hops: [PUBLISHER_B], instance: named(version, "b") },
				version,
			),
		);
		await announced.next();
	};
	return { withdraw, replace: () => withdraw().then(announce) };
}

// Write the subscribe responses to the subscriber, FINishing the stream when asked.
async function respond(stream: FakeStream, version: Version, responses: SubscribeResponse[], fin = false) {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
		version,
	);
	for (const response of responses) await encodeSubscribeResponse(writer, response, version);
	for (const chunk of chunks) stream.inbound.enqueue(chunk);
	if (fin) stream.inbound.close();
}

// A SUBSCRIBE without an instance resolves whichever instance won the path when it arrived, so one
// unanswered when another instance takes the path is refused, a withdrawal followed by a new
// announcement included. One answered first stays on its instance, and a withdrawal alone leaves
// it to its answer, so a track that finished as its broadcast ended still arrives.
test.each([
	["unanswered on lite-05 is refused", Version.DRAFT_05, "replaced"],
	["unanswered on lite-06 is refused", Version.DRAFT_06, "replaced"],
	["answered on lite-06 stays", Version.DRAFT_06, "answered"],
	["unanswered on lite-05 finishes", Version.DRAFT_05, "withdrawn"],
	["unanswered on lite-06 finishes", Version.DRAFT_06, "withdrawn"],
] as const)("an unpinned SUBSCRIBE %s at a replacement or withdrawal", async (_, version, outcome) => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const announced = subscriber.routes();
	try {
		await opened(0);
		const pool = await unpinnedPool(version, streams, announced);
		const held = subscriber.consume(Path.from("pool/job"));
		const track = held.track("video").subscribe();
		await answerTrackInfo(await opened(1), version);
		const subscribe = await opened(2);
		await subscribe.reading;

		if (outcome === "answered") {
			await respond(subscribe, version, [{ start: new SubscribeStart([0n]) }]);
			await drainReactions();
			await pool.replace();
			await drainReactions();
			expect(track.closed.peek()).toBeUndefined();
		} else if (outcome === "withdrawn") {
			await pool.withdraw();
			await drainReactions();
			expect(track.closed.peek()).toBeUndefined();
			await respond(
				subscribe,
				version,
				[{ start: new SubscribeStart([0n]) }, { finish: new SubscribeFinish(0n) }],
				true,
			);
			expect(await track.closed).toBeNull();
		} else {
			await pool.replace();
			const err = await track.closed;
			expect(err).toBeInstanceOf(StreamError);
			expect((err as StreamError).code).toBe(StreamCode.Unroutable);
			await subscribe.aborted;
		}
		track.close();
		held.close();
	} finally {
		announced.close();
		subscriber.close();
		warn.mockRestore();
	}
});

// A TRACK answered while its SUBSCRIBE still waits for a stream belongs to the subscription, so
// a withdrawal then closes it rather than leaving it to a create that may never open. Only a
// minted instance ends with its route: a named one leaves the request to the peer.
test("a withdrawal while the subscribe opens closes the answered track", async () => {
	const version = Version.DRAFT_06;
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession([2]);
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const announced = subscriber.routes();
	try {
		await opened(0);
		const pool = await unpinnedPool(version, streams, announced);
		const held = subscriber.consume(Path.from("pool/job"));
		const track = held.track("video").subscribe();
		const info = await opened(1);
		await opened(2);
		await info.reading;
		await answerTrackInfo(info, version);
		await drainReactions();

		let finished = false;
		void info.finished.then(() => {
			finished = true;
		});
		await pool.withdraw();
		expect(await track.closed).toBeInstanceOf(Error);
		await drainReactions();
		expect(finished).toBe(true);
		track.close();
		held.close();
	} finally {
		announced.close();
		subscriber.close();
		warn.mockRestore();
	}
});

test("a listing whose readers leave as its token settles never reaches the wire", async () => {
	const { quic, streams } = fakeSession();
	let release!: () => void;
	const released = new Promise<void>((resolve) => (release = resolve));
	let ask!: () => void;
	const asked = new Promise<void>((resolve) => (ask = resolve));
	// Ignores `until`, so the wait settles admitted after the readers left.
	const auth = {
		ready: () => {
			ask();
			return released.then(() => true);
		},
	} as unknown as AuthSession;
	const subscriber = new Subscriber(
		quic,
		Version.DRAFT_07,
		HopSchema.parse(1n),
		undefined,
		undefined,
		undefined,
		undefined,
		undefined,
		auth,
	);
	const held = subscriber.consume(Path.from("room"));
	const tracks = held.tracks();
	await asked;
	release();
	tracks.close();
	await yieldNow();
	expect(streams.length).toBe(0);
	held.close();
	subscriber.close();
});

test.each(["track", "fetch", "open"] as const)(
	"retirement settles coalesced fetches during %s setup",
	async (stage) => {
		const { quic, streams, opened } = fakeSession(stage === "open" ? [1] : []);
		const subscriber = new Subscriber(quic, Version.DRAFT_06, HopSchema.parse(1n));
		const retired = new AbortController();
		const consumed = { id: 1, valid: retired.signal };
		const path = Path.from("room");
		const first = subscriber.fetchGroup(path, "video", 0n, {}, consumed).catch((err: unknown) => err);
		const second = subscriber.fetchGroup(path, "video", 0n, {}, consumed).catch((err: unknown) => err);
		await opened(0);
		if (stage !== "track") {
			await answerTrackInfo(streams[0], Version.DRAFT_06);
			await opened(1);
		}
		const cause = new Error("broadcast instance retired");
		retired.abort(cause);
		expect(await first).toBe(cause);
		expect(await second).toBe(cause);
		if (stage === "open") {
			// The FETCH open left the line: the next open takes its stream rather than resetting it.
			streams[1].release();
			await drainReactions();
			const next = subscriber.fetchGroup(path, "video", 1n).catch((err: unknown) => err);
			await streams[1].writing;
			subscriber.close();
			await next;
			return;
		}
		const active = streams[stage === "track" ? 0 : 1];
		expect(await active.aborted).toBeDefined();
		subscriber.close();
	},
);

test("retirement preserves an accepted fetch and its coalesced reads", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_06, HopSchema.parse(1n));
	const retired = new AbortController();
	const consumed = { id: 1, valid: retired.signal };
	const path = Path.from("room");
	const fetch = subscriber.fetchGroup(path, "video", 0n, {}, consumed);
	await opened(0);
	await answerTrackInfo(streams[0], Version.DRAFT_06);
	await opened(1);
	await streams[1].reading;
	streams[1].inbound.enqueue(new Uint8Array([0, 4, ...new TextEncoder().encode("old!")]));
	const first = await fetch;
	retired.abort(new Error("broadcast instance retired"));
	const cached = await subscriber.fetchGroup(path, "video", 0n, {}, consumed);
	expect(streams.length).toBe(2);
	expect((await first.readFrame())?.payload).toEqual((await cached.readFrame())?.payload);
	first.close();
	cached.close();
	subscriber.close();
});

// Drive discovery and inspect the TRACK request before answering it. Waiting for the
// reader means the whole request has reached the wire, without a clock-based settle.
test("consumes resolve the serving prefix and pin its instance", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const announced = subscriber.routes();
	await opened(0);
	const peer = new Writer(
		new WritableStream<Uint8Array>({
			write: (chunk) => streams[0].inbound.enqueue(new Uint8Array(chunk)),
		}),
		version,
	);
	await new AnnounceOk(PEER, 0).encode(peer, version);
	const pool = Path.from("pool");
	const job = Path.from("pool/job");
	const instance = Instance.random();
	const claim = claimer(version);
	await claim(peer, pool, { instance, hops: [], node: NODE_A });
	await announced.next();
	const held = subscriber.consume(job);
	const info = held
		.track("video")
		.info()
		.catch(() => {});
	await opened(1);
	await streams[1].reading;
	const reader = new Reader(undefined, new Uint8Array(Buffer.concat(streams[1].written)), version);
	expect(await reader.u53()).toBe(StreamId.Track);
	expect(await Track.decode(reader, version)).toMatchObject({ instance, node: NODE_A });

	// The more specific source's instance shadows the covering source's.
	const other = Instance.random();
	await claim(peer, job, { instance: other, hops: [], node: NODE_B });
	await announced.next();
	const fresh = subscriber.consume(job);
	expect(fresh.closed).not.toBe(held.closed);
	expect(held.closed.peek()).toBeUndefined();
	const freshInfo = fresh
		.track("video")
		.info()
		.catch(() => {});
	await opened(2);
	await streams[2].reading;
	const freshReader = new Reader(undefined, new Uint8Array(Buffer.concat(streams[2].written)), version);
	await freshReader.u53();
	expect(await Track.decode(freshReader, version)).toMatchObject({ instance: other, node: NODE_B });

	await encodeAnnounceBroadcast(peer, { status: "endedId", id: 1n }, version);
	await encodeAnnounceBroadcast(peer, { status: "floor", id: 1n }, version);
	await announced.next();
	const fallback = subscriber.consume(job);
	expect(fallback.closed).toBe(held.closed);
	expect(subscriber.consume(pool).closed).toBe(subscriber.consume(pool).closed);
	fresh.close();
	held.close();
	fallback.close();
	announced.close();
	subscriber.close();
	await Promise.all([info, freshInfo]);
});

test.each([Version.DRAFT_06, Version.DRAFT_07])(
	"a more specific start replaces an existing consume on %s",
	async (version) => {
		const { subscriber, send, settle } = announceHarness(version);
		const announced = subscriber.routes();
		await settle();
		await send((w) => new AnnounceOk(PEER, 0).encode(w, version));
		const pool = Path.from("pool");
		const job = Path.from("pool/job");
		const claim = claimer(version);
		await send((w) => claim(w, pool, { instance: named(version, "a"), hops: [PUBLISHER_A], node: NODE_A }));
		await announced.next();
		const held = subscriber.consume(job);
		await send((w) => claim(w, job, { instance: named(version, "b"), hops: [PUBLISHER_B], node: NODE_B }));
		await announced.next();
		const fresh = subscriber.consume(job);
		expect(fresh.closed).not.toBe(held.closed);
		expect(held.closed.peek()).toBeUndefined();
		fresh.close();
		held.close();
		announced.close();
		subscriber.close();
	},
);

// Only a wire without instances: on lite-07 the request pins its instance, which the peer
// answers for whatever else is selected meanwhile.
test.each([Version.DRAFT_05, Version.DRAFT_06])(
	"pending metadata cancels on selection change and a live fallback can request again on %s",
	async (version) => {
		const { quic, streams, opened } = fakeSession();
		const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
		const announced = subscriber.routes();
		await opened(0);
		const peer = new Writer(
			new WritableStream<Uint8Array>({
				write: (chunk) => streams[0].inbound.enqueue(new Uint8Array(chunk)),
			}),
			version,
		);
		await new AnnounceOk(PEER, 0).encode(peer, version);
		const pool = Path.from("pool");
		const job = Path.from("pool/job");
		await encodeAnnounceBroadcast(
			peer,
			{ status: "active", suffix: pool, instance: named(version, "a"), hops: [PUBLISHER_A] },
			version,
		);
		await announced.next();
		const held = subscriber.consume(job);
		const pending = held
			.track("pending")
			.info()
			.catch((err: unknown) => err);
		await opened(1);
		await streams[1].reading;
		await encodeAnnounceBroadcast(
			peer,
			{ status: "active", suffix: job, instance: named(version, "b"), hops: [PUBLISHER_B] },
			version,
		);
		await announced.next();
		expect(await pending).toBeInstanceOf(Error);
		await streams[1].aborted;
		await expect(held.track("during").info()).rejects.toThrow();
		expect(streams.length).toBe(2);
		await encodeAnnounceBroadcast(
			peer,
			version === Version.DRAFT_05 ? { status: "ended", suffix: job } : { status: "endedId", id: 1n },
			version,
		);
		await announced.next();
		const info = held.track("after").info();
		await opened(2);
		await answerTrackInfo(streams[2], version);
		expect((await info).priority).toBe(0);
		held.close();
		announced.close();
		subscriber.close();
	},
);

test("a cancelled queued PROBE open sends no request and leaves its slot to the next open", async () => {
	const { quic, streams, opened } = fakeSession([0]);
	const signal = new Signal<ProbeStats>({});
	const subscriber = new Subscriber(quic, Version.DRAFT_03, HopSchema.parse(1n), signal);
	const abort = new AbortController();
	const probing = subscriber.runProbe(abort.signal);
	await opened(0);
	abort.abort();
	await probing;
	expect(streams[0].written).toEqual([]);
	streams[0].release();
	await drainReactions();
	expect(streams[0].written).toEqual([]);

	// The next open takes the stream the cancelled one left, rather than another create.
	const again = new AbortController();
	const reprobing = subscriber.runProbe(again.signal);
	await streams[0].writing;
	expect(streams).toHaveLength(1);
	again.abort();
	await reprobing;
	subscriber.close();
});

// Let all queued reactions finish without relying on host timers.
function drainReactions(): Promise<void> {
	return new Promise((resolve) => {
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			resolve();
		};
		channel.port2.postMessage(null);
	});
}

test("a local unsubscribe owes no SUBSCRIBE_FINISH", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	const responseTask = Promise.withResolvers<void>();
	const outcome = responseTask.promise.then(
		() => undefined,
		(err: unknown) => err,
	);
	// TypeScript-private so tests can observe the task without exporting a test hook.
	const internal = subscriber as unknown as { runResponses: (...args: unknown[]) => Promise<void> };
	const runResponses = internal.runResponses.bind(internal);
	const responses = spyOn(internal, "runResponses").mockImplementation((...args) => {
		const task = runResponses(...args);
		responseTask.resolve(task);
		return task;
	});
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	const ordered = track.ordered();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], Version.DRAFT_07);
		await (await opened(1)).reading;
		const next = ordered.nextGroup();
		await subscriber.runGroup(
			new Group({ subscribe: 0n, sequence: 0n }),
			new Reader(
				new ReadableStream({
					start: (controller) => controller.close(),
				}),
				undefined,
				Version.DRAFT_07,
			),
		);
		(await next)?.close();
		ordered.close();
		track.close();
		await streams[1].finished;
		await drainReactions();
		expect(await outcome).toBeUndefined();
	} finally {
		responses.mockRestore();
		ordered.close();
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// Count the actual source registrations, including promises whose losing race cannot cancel them.
function countChanges(source: Getter<unknown>) {
	const changed = source.changed.bind(source);
	let listeners = 0;
	const register = (fn: (value: unknown) => void) => {
		listeners++;
		let live = true;
		const release = () => {
			if (!live) return;
			live = false;
			listeners--;
		};
		const dispose = changed((value) => {
			release();
			fn(value);
		});
		return () => {
			release();
			dispose();
		};
	};
	const spy = spyOn(source, "changed").mockImplementation(((fn?: (value: unknown) => void) => {
		return fn ? register(fn) : new Promise(register);
	}) as typeof source.changed);
	return { count: () => listeners, restore: () => spy.mockRestore() };
}

test.each(
	[Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07].flatMap((version) =>
		["unsubscribe", "reset", "session"].map((stop) => [version, stop] as const),
	),
)("subscription waits release losing signal listeners on %s after %s", async (version, stop) => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const counters: ReturnType<typeof countChanges>[] = [];
	// Observe the accepted producer before its update loop starts, without exporting a test hook.
	const internal = subscriber as unknown as {
		runResponses: (stream: unknown, entry: { track: Producer }) => Promise<void>;
	};
	const runResponses = internal.runResponses.bind(internal);
	const responses = spyOn(internal, "runResponses").mockImplementation((stream, entry) => {
		counters.push(
			...[hooks.requested(entry.track), entry.track.demand().wanted, hooks.producerLargest(entry.track)].map(
				countChanges,
			),
		);
		return runResponses(stream, entry);
	});
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		// Timed, since every group stream below carries a timestamp.
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;
		await drainReactions();
		const before = counters.map((counter) => counter.count());
		expect(before).toHaveLength(3);
		expect(before.every((count) => count > 0)).toBe(true);
		for (let sequence = 0n; sequence < 50n; sequence++) {
			await subscriber.runGroup(
				new Group({ subscribe: 0n, sequence }),
				await groupStream(Number(sequence), version),
			);
			await drainReactions();
		}
		// Quiet subscription and demand signals must not retain a listener per received group.
		expect(counters.map((counter) => counter.count())).toEqual(before);
		if (stop === "unsubscribe") track.close();
		else if (stop === "reset") streams[1].inbound.error(new Error("peer reset"));
		else subscriber.close();
		await drainReactions();
		expect(counters.map((counter) => counter.count())).toEqual([0, 0, 0]);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
		await drainReactions();
		responses.mockRestore();
		for (const counter of counters) counter.restore();
	}
});

test.each([0, 5, 9000])("a received group numbers its frames from transport offset %s", async (frameStart) => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const consumer = broadcast.track("video");
	const early = consumer.subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;
		await subscriber.runGroup(
			new Group({ subscribe: 0n, sequence: 0n, frameStart }),
			await groupStream(1000, version),
		);
		const late = consumer.subscribe();
		try {
			for (const reader of [early, late]) {
				const group = await reader.recvGroup();
				if (!group) throw new Error("no received group");
				expect((await group.readFrameSequence())?.sequence).toBe(frameStart);
				expect(await group.readFrameSequence()).toBeUndefined();
				group.close();
			}
		} finally {
			late.close();
		}
	} finally {
		early.close();
		broadcast.close();
		subscriber.close();
	}
});

// A group stream carrying one frame at `timestamp` (zigzag delta from 0) in the track's scale.
async function groupStream(timestamp: number, version: Version = Version.DRAFT_07): Promise<Reader> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
		version,
	);
	// Lite-07 leads with the size plus one; earlier versions end the header with the size.
	const lengthFirst = hasLengthFirst(version);
	if (lengthFirst) await writer.u53(2);
	await writer.u62(BigInt(timestamp) * 2n);
	if (!lengthFirst) await writer.u53(1);
	await writer.write(new Uint8Array([0]));
	return new Reader(
		new ReadableStream({
			start: (controller) => {
				for (const chunk of chunks) controller.enqueue(chunk);
				controller.close();
			},
		}),
		undefined,
		version,
	);
}

// Lite-05/06 stamp an untimed track's frames with their send time, which a receiver cannot tell
// from media time, so it keeps every timestamp there unchecked: a group that rewinds or lands out
// of order is accepted with its timestamp.
test.each([Version.DRAFT_05, Version.DRAFT_06])("version %p passes received timestamps through", async (version) => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	// Old enough to keep every group, however far behind the newest start.
	const track = broadcast.track("video").subscribe({ delay: { max: Duration.millis(60_000) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 4n }), await groupStream(1000, version));
		// Starts before the group below it.
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 5n }), await groupStream(500, version));
		// Lands below group 5 but starts after it.
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 3n }), await groupStream(2000, version));
		const got = [];
		for (let i = 0; i < 3; i++) {
			const group = await track.recvGroup();
			const frame = await group?.readFrame();
			got.push([group?.sequence, frame?.payload.length, frame?.timestamp?.value]);
		}
		expect(got).toEqual([
			[3n, 1, 2000],
			[4n, 1, 1000],
			[5n, 1, 500],
		]);
		expect((await track.info()).timescale).toBe(Timescale.MILLI);
		expect(track.closed.peek()).toBeUndefined();
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// Lite-07 timestamps are media time, so a received group is checked like a local write.
test("a lite-07 rewind is refused", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe({ delay: { max: Duration.millis(60_000) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 4n }), await groupStream(1000, version));
		// Starts before the group below it, so its stream is stopped with the refusal.
		const rewound = await groupStream(500, version);
		const stop = spyOn(rewound, "stop");
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 5n }), rewound);
		expect(stop.mock.calls[0]?.[0]).toBeInstanceOf(TimestampMismatch);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

test("a publisher FIN without SUBSCRIBE_FINISH still violates an open subscription", async () => {
	const { quic, streams, opened, closes } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], Version.DRAFT_07);
		await (await opened(1)).reading;
		streams[1].inbound.close();
		expect(await track.closed).toBeInstanceOf(ProtocolViolation);
		expect((track.closed.peek() as Error).message).toBe("subscribe stream ended without SUBSCRIBE_FINISH");
		await drainReactions();
		expect(closes.map((info) => info?.closeCode)).toEqual([SessionCode.ProtocolViolation]);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// Our SUBSCRIBE carries one range, so a lite-07 SUBSCRIBE_START with any other count is malformed.
test.each([[[]], [[0, 5]]])("a lite-07 SUBSCRIBE_START resolving %j violates the protocol", async (starts) => {
	const { quic, streams, opened, closes } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], Version.DRAFT_07);
		const request = await opened(1);
		await request.reading;
		await respond(request, Version.DRAFT_07, [{ start: new SubscribeStart(starts.map(BigInt)) }]);
		expect(await track.closed).toBeInstanceOf(ProtocolViolation);
		await drainReactions();
		expect(closes.map((info) => info?.closeCode)).toEqual([SessionCode.ProtocolViolation]);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// An empty START resolves the feed's start, and a range an update adds leaves it pending until its
// SUBSCRIBE_STARTED says where that range starts, which the feed then starts from.
test("a widened range's SUBSCRIBE_STARTED resolves the feed's start", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe({ ranges: [{ start: { absolute: 0n }, end: { absolute: 1n } }] });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		const request = await opened(1);
		await request.reading;
		expect(hooks.sourceStart(track)).toBe("pending");
		await respond(request, version, [{ start: new SubscribeStart([undefined]) }]);
		await drainReactions();
		expect(hooks.sourceStart(track)).not.toBe("pending");
		track.update({ ranges: [{ start: { absolute: 0n }, end: { absolute: 2n } }] });
		await drainReactions();
		expect(hooks.sourceStart(track)).toBe("pending");
		// The widened range took the next Range ID.
		await respond(request, version, [{ started: new SubscribeStarted(1n, 1n) }]);
		await drainReactions();
		expect(await track.start()).toBe(1n);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// A group that arrives over a lite-07 stale (OLD) drop wins over it, whichever lands first: the
// publisher serves a group after its stale drop, as a front that switched routes does, and the
// group's stream may overtake the drop. The next update owes it no more, and a group still
// arriving is never aborted by the drop. Mirrors the Rust `a_group_wins_over_its_stale_drop`.
test("a group wins over its stale drop on lite-07", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	type Entry = {
		track: Producer;
		tail: { covers(start: bigint, end: bigint): boolean; open(sequence: bigint): () => void };
	};
	let entry: Entry | undefined;
	const internal = subscriber as unknown as {
		runResponses: (stream: unknown, entry: Entry) => Promise<void>;
	};
	const runResponses = internal.runResponses.bind(internal);
	const responses = spyOn(internal, "runResponses").mockImplementation((stream, captured) => {
		entry = captured;
		return runResponses(stream, captured);
	});
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe({ delay: { max: Duration.millis(500) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		const stream = await opened(1);
		await stream.reading;
		const send = async (...resps: SubscribeResponse[]) => {
			const chunks: Uint8Array[] = [];
			const writer = new Writer(
				new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
				version,
			);
			for (const resp of resps) await encodeSubscribeResponse(writer, resp, version);
			for (const chunk of chunks) stream.inbound.enqueue(chunk);
			await drainReactions();
		};
		const old = (sequence: bigint): SubscribeResponse => ({
			drop: new SubscribeDrop({ start: sequence, end: sequence + 1n, error: StreamCode.Old }),
		});
		// A group stream's header: the tail accounts for it, and the track holds the group.
		const arrive = (sequence: bigint) => {
			if (!entry) throw new Error("no subscription");
			const read = entry.tail.open(sequence);
			return { group: hooks.receiveGroup(entry.track, { sequence, start: 0 }), read };
		};

		await send({ start: new SubscribeStart([0n]) });
		// OLD, then the group, which finishes.
		await send(old(1n));
		const one = arrive(1n);
		one.group.finish();
		one.read();
		// The group first, still arriving when its OLD lands.
		const two = arrive(2n);
		await send(old(2n));
		// OLD, then the group, still arriving at the update.
		await send(old(3n));
		const three = arrive(3n);
		// OLD, and no group.
		await send(old(4n));
		if (!entry) throw new Error("no subscription");

		track.update({ priority: 1, delay: { max: Duration.millis(500) } });
		await drainReactions();
		// Every group that arrived stays accounted for; the one that never arrived is owed again.
		expect(entry.tail.covers(1n, 4n)).toBe(true);
		expect(entry.tail.covers(4n, 5n)).toBe(false);
		// Neither the drop nor the update aborts a group still arriving.
		expect(two.group.closed.peek()).toBeUndefined();
		expect(three.group.closed.peek()).toBeUndefined();
		two.read();
		three.read();
	} finally {
		responses.mockRestore();
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// A group whose stream finished wins over a lite-07 stale drop that lands after the cache evicted
// it: the publisher served it over that drop, so the next update owes it no more. A stream that
// finished under an earlier update says nothing of a drop under the new one. Mirrors the Rust
// `a_finished_group_wins_over_a_late_stale_drop_once_evicted`.
test("a finished group wins over a late stale drop once evicted on lite-07", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	type Entry = {
		track: Producer;
		tail: {
			covers(start: bigint, end: bigint): boolean;
			fold(): boolean;
		};
	};
	let entry: Entry | undefined;
	const internal = subscriber as unknown as {
		runResponses: (stream: unknown, entry: Entry) => Promise<void>;
	};
	const runResponses = internal.runResponses.bind(internal);
	const responses = spyOn(internal, "runResponses").mockImplementation((stream, captured) => {
		entry = captured;
		return runResponses(stream, captured);
	});
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe({ delay: { max: Duration.millis(500) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		const stream = await opened(1);
		await stream.reading;
		const send = async (...resps: SubscribeResponse[]) => {
			const chunks: Uint8Array[] = [];
			const writer = new Writer(
				new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
				version,
			);
			for (const resp of resps) await encodeSubscribeResponse(writer, resp, version);
			for (const chunk of chunks) stream.inbound.enqueue(chunk);
			await drainReactions();
		};
		const old = (sequence: bigint, update: bigint): SubscribeResponse => ({
			drop: new SubscribeDrop({ start: sequence, end: sequence + 1n, error: StreamCode.Old, update }),
		});
		// A group stream that arrives whole, through the path that records it.
		const arrive = (sequence: bigint) =>
			subscriber.runGroup(
				new Group({ subscribe: 0n, sequence }),
				new Reader(new ReadableStream({ start: (controller) => controller.close() }), undefined, version),
			);
		// Everything below the live edge ages out of the cache.
		const evict = () => clock.advance(DEFAULT_EXPIRY.mul(2));
		let priority = 0;
		const update = async () => {
			track.update({ priority: ++priority, delay: { max: Duration.millis(500) } });
			await drainReactions();
		};

		await send({ start: new SubscribeStart([0n]) });
		await arrive(1n);
		await arrive(10n);
		await evict();
		await send(old(1n, 0n));
		await update();
		if (!entry) throw new Error("no subscription");
		expect(entry.tail.covers(1n, 2n)).toBe(true);

		// Finished under the first update, then judged stale under the second.
		await arrive(2n);
		await update();
		await evict();
		await send(old(2n, 2n));
		await update();
		expect(entry.tail.covers(2n, 3n)).toBe(false);

		// Under the pool's debt the tail folds what finished, gaps and all. A group in the span whose
		// stream arrived before its stale drop still wins over it, as does a gap the grace gave up
		// on; a gap there the drop accounted first is owed again.
		for (const sequence of [3n, 5n]) await arrive(sequence);
		await evict();
		for (const sequence of [11n, 13n, 15n]) await arrive(sequence);
		expect(entry.tail.fold()).toBe(true);
		await send(old(3n, 3n), old(4n, 3n), old(12n, 3n));
		await update();
		expect(entry.tail.covers(3n, 5n)).toBe(true);
		expect(entry.tail.covers(12n, 13n)).toBe(false);
	} finally {
		responses.mockRestore();
		track.close();
		broadcast.close();
		subscriber.close();
		installed.close();
	}
});

// A SKIP, or any final drop, that follows an OLD drop of the same groups replaces it: an update
// reopens only stale verdicts, so the groups stay accounted for, and the skip stays final rather
// than being superseded with the OLD drop. An empty range names nothing. Mirrors Rust.
test("a skip or final drop replaces an earlier stale one on lite-07", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	type Entry = { track: Producer; tail: { covers(start: bigint, end: bigint): boolean } };
	let entry: Entry | undefined;
	const internal = subscriber as unknown as {
		runResponses: (stream: unknown, entry: Entry) => Promise<void>;
	};
	const runResponses = internal.runResponses.bind(internal);
	const responses = spyOn(internal, "runResponses").mockImplementation((stream, captured) => {
		entry = captured;
		return runResponses(stream, captured);
	});
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe({ delay: { max: Duration.millis(500) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		const stream = await opened(1);
		await stream.reading;
		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);
		const old = (start: bigint, end: bigint): SubscribeResponse => ({
			drop: new SubscribeDrop({ start, end, error: StreamCode.Old }),
		});
		for (const resp of [
			{ start: new SubscribeStart([0n]) },
			old(1n, 3n),
			old(4n, 5n),
			{ skip: new SubscribeSkip({ start: 1n, end: 3n }) },
			{ drop: new SubscribeDrop({ start: 4n, end: 5n, error: StreamCode.NotFound }) },
			// Empty ranges.
			old(6n, 6n),
			{ skip: new SubscribeSkip({ start: 6n, end: 6n }) },
		] satisfies SubscribeResponse[]) {
			await encodeSubscribeResponse(writer, resp, version);
		}
		for (const chunk of chunks) stream.inbound.enqueue(chunk);
		await drainReactions();
		if (!entry) throw new Error("no subscription");

		track.update({ priority: 1, delay: { max: Duration.millis(500) } });
		await drainReactions();
		expect(entry.tail.covers(1n, 3n)).toBe(true);
		expect(entry.tail.covers(4n, 5n)).toBe(true);
		// Still skipped after the update.
		expect(() => entry?.track.createGroup(1n)).toThrow(Skipped);
	} finally {
		responses.mockRestore();
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// A SKIP replaces an earlier final DROP of the same groups too: "never produced" is the most
// authoritative verdict. A later DROP leaves the skip in place, and a SKIP naming a group that
// arrived keeps the group, logging a protocol error without ending the session. Mirrors Rust.
test("a skip replaces a final drop but not a cached group on lite-07", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	type Entry = { track: Producer; tail: { covers(start: bigint, end: bigint): boolean } };
	let entry: Entry | undefined;
	const internal = subscriber as unknown as {
		runResponses: (stream: unknown, entry: Entry) => Promise<void>;
	};
	const runResponses = internal.runResponses.bind(internal);
	const responses = spyOn(internal, "runResponses").mockImplementation((stream, captured) => {
		entry = captured;
		return runResponses(stream, captured);
	});
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe({ delay: { max: Duration.millis(500) } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		const stream = await opened(1);
		await stream.reading;
		const send = async (resps: SubscribeResponse[]) => {
			const chunks: Uint8Array[] = [];
			const writer = new Writer(
				new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
				version,
			);
			for (const resp of resps) await encodeSubscribeResponse(writer, resp, version);
			for (const chunk of chunks) stream.inbound.enqueue(chunk);
			await drainReactions();
		};
		await send([
			{ start: new SubscribeStart([0n]) },
			{ drop: new SubscribeDrop({ start: 1n, end: 3n, error: StreamCode.NotFound }) },
		]);
		if (!entry) throw new Error("no subscription");
		entry.track.createGroup(5n).finish();

		await send([
			{ skip: new SubscribeSkip({ start: 1n, end: 3n }) },
			// A later drop leaves the skip in place.
			{ drop: new SubscribeDrop({ start: 2n, end: 3n, error: StreamCode.NotFound }) },
			// Group 5 arrived, so the skip names only 4 and 6.
			{ skip: new SubscribeSkip({ start: 4n, end: 7n }) },
		]);
		expect(() => entry?.track.createGroup(1n)).toThrow(Skipped);
		expect(() => entry?.track.createGroup(2n)).toThrow(Skipped);
		expect(entry.tail.covers(4n, 7n)).toBe(true);
		expect(warn).toHaveBeenCalledTimes(1);

		const reader = entry.track.subscribe({ delay: { max: null } });
		const got: unknown[] = [];
		for (
			let next = hooks.tryRecvGroup(reader, true);
			next.kind !== "idle";
			next = hooks.tryRecvGroup(reader, true)
		) {
			if (next.kind === "group") got.push(next.group.sequence);
			else if (next.kind === "drop") got.push([next.start, next.end, next.skip === true]);
			else break;
		}
		expect(got).toEqual([[1n, 3n, true], [4n, 5n, true], [6n, 7n, true], 5n]);
		reader.close();
	} finally {
		warn.mockRestore();
		responses.mockRestore();
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// Before lite-05 the publisher FINs the subscribe stream only once the track is over, so that FIN
// finishes the track once the tail settles, rather than releasing it for another route.
test.each(
	[Version.DRAFT_01, Version.DRAFT_02, Version.DRAFT_03, Version.DRAFT_04].map((v) => [versionName(v), v] as const),
)("a %s publisher FIN finishes the track", async (_, version) => {
	const clock = new Manual();
	const installed = clock.install();
	const { quic, streams, opened, closes } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		const stream = await opened(0);
		await stream.reading;
		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);
		await encodeSubscribeResponse(writer, { ok: new SubscribeOk({}) }, version);
		for (const chunk of chunks) streams[0].inbound.enqueue(chunk);
		streams[0].inbound.close();
		await drainReactions();

		await clock.advance(GRACE);
		expect(await track.finished()).toEqual({ group: 0n, frame: 0 });
		expect(await track.closed).toBeNull();
		expect(closes).toEqual([]);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
		installed.close();
	}
});

// A session that dies once the publisher ended the track, but before the tail settled, cuts it
// off: the track ends with the session's error unless the declared end settled first. That end
// is SUBSCRIBE_FINISH on lite-05+, and the FIN before it.
test.each([
	["lite-03 ends clean with its last group finished", Version.DRAFT_03, true],
	["lite-03 aborts with its last group still open", Version.DRAFT_03, false],
	["lite-05 ends clean with its last group finished", Version.DRAFT_05, true],
	["lite-05 aborts with its last group still owed", Version.DRAFT_05, false],
] as const)("a session death before the tail settles: %s", async (_, version, settled) => {
	const installed = new Manual().install();
	const { quic, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	const died = new Error("session died");
	// Lite-05 asks for TRACK_INFO first; untimed, a frame is its size and payload.
	const legacy = version === Version.DRAFT_03;
	const frame = (controller: ReadableStreamDefaultController<Uint8Array>) =>
		controller.enqueue(new Uint8Array([1, 0]));
	try {
		if (!legacy) await answerTrackInfo(await opened(0), version);
		const subscribe = await opened(legacy ? 0 : 1);
		await subscribe.reading;
		const responses: SubscribeResponse[] = legacy
			? [{ ok: new SubscribeOk({}) }]
			: [{ start: new SubscribeStart([0n]) }, { finish: new SubscribeFinish(1n) }];
		await respond(subscribe, version, responses, true);
		if (settled) {
			const group = new ReadableStream<Uint8Array>({
				start: (controller) => {
					frame(controller);
					controller.close();
				},
			});
			await subscriber.runGroup(
				new Group({ subscribe: 0n, sequence: 0n }),
				new Reader(group, undefined, version),
			);
		} else if (legacy) {
			// Lite-03 says nothing about which groups are owed, so only a stream still being read is.
			const group = new ReadableStream<Uint8Array>({ start: frame });
			void subscriber.runGroup(new Group({ subscribe: 0n, sequence: 0n }), new Reader(group, undefined, version));
		}
		await drainReactions();

		subscriber.close(died);
		expect(await track.closed).toBe(settled ? null : died);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
		installed.close();
	}
});

// A publisher that finishes the track stops reading its subscribe stream, so a SUBSCRIBE_UPDATE
// written then fails. The track still ends with the publisher's finish; a reset still fails it.
test.each([
	["lite-05 finish", Version.DRAFT_05, "finish"],
	["lite-06 finish", Version.DRAFT_06, "finish"],
	["lite-07 finish", Version.DRAFT_07, "finish"],
	["lite-07 reset", Version.DRAFT_07, "reset"],
] as const)("an update the publisher stopped reading defers to its %s", async (_, version, end) => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		const stream = await opened(1);
		await stream.reading;

		const stopped = Object.assign(new Error("The stream was aborted by the remote server"), {
			source: "stream",
			streamErrorCode: StreamCode.Cancel,
		});
		const refused = stream.stop(stopped);
		track.update({ priority: 1 });
		await refused;
		await drainReactions();
		expect(track.closed.peek()).toBeUndefined();

		if (end === "finish") {
			const chunks: Uint8Array[] = [];
			const writer = new Writer(
				new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
				version,
			);
			await encodeSubscribeResponse(writer, { finish: new SubscribeFinish(0n) }, version);
			for (const chunk of chunks) stream.inbound.enqueue(chunk);
			stream.inbound.close();
			expect(await track.closed).toBeNull();
		} else {
			stream.inbound.error(
				Object.assign(new Error("reset"), { source: "stream", streamErrorCode: StreamCode.NotFound }),
			);
			const err = await track.closed;
			expect(err).toBeInstanceOf(StreamError);
			expect((err as StreamError).code).toBe(StreamCode.NotFound);
		}
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// Only a failed write defers to the responses: an update the wire can't carry still fails the
// track. Lite-05 sends the start group plus one, past the largest u62 here.
test("an update whose bound the wire can't carry fails the track", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], Version.DRAFT_05);
		await (await opened(1)).reading;
		track.update({ ranges: [{ start: { absolute: 2n ** 62n - 1n } }] });
		expect(await track.closed).toBeInstanceOf(RangeError);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// A SUBSCRIBE_DROP can only echo an update the subscriber sent: one naming a later update is a
// protocol violation. Mirrors the Rust `a_drop_naming_an_unsent_update_is_a_protocol_violation`.
test("a lite-07 drop naming an unsent update closes the session", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened, closes } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		const stream = await opened(1);
		await stream.reading;
		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);
		await encodeSubscribeResponse(writer, { start: new SubscribeStart([0n]) }, version);
		await encodeSubscribeResponse(
			writer,
			{ drop: new SubscribeDrop({ start: 1n, end: 2n, error: StreamCode.Old, update: 1n }) },
			version,
		);
		for (const chunk of chunks) stream.inbound.enqueue(chunk);
		expect(await track.closed).toBeInstanceOf(ProtocolViolation);
		await drainReactions();
		expect(closes.map((info) => info?.closeCode)).toEqual([SessionCode.ProtocolViolation]);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

test("an unknown subscribe response type closes the session", async () => {
	const { quic, streams, opened, closes } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], Version.DRAFT_07);
		await (await opened(1)).reading;
		// An unassigned response type is a session protocol violation.
		streams[1].inbound.enqueue(new Uint8Array([5, 0]));
		expect(await track.closed).toBeInstanceOf(ProtocolViolation);
		await drainReactions();
		expect(closes.map((info) => info?.closeCode)).toEqual([SessionCode.ProtocolViolation]);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

test("closing a broadcast while TRACK_INFO is pending leaves no unhandled rejection", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	const unhandled: unknown[] = [];
	const onUnhandled = (reason: unknown) => unhandled.push(reason);
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	process.on("unhandledRejection", onUnhandled);
	try {
		await opened(0);
		await streams[0].reading;
		broadcast.close();
		track.close();
		await streams[0].aborted;
		await streams[1].aborted;
		await drainReactions();
		expect(streams).toHaveLength(2);
		expect(unhandled).toEqual([]);
	} finally {
		process.off("unhandledRejection", onUnhandled);
		subscriber.close();
	}
});

const announceStart = (suffix: string) => (w: Writer) =>
	encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from(suffix), hops: [PUBLISHER_A] }, Version.DRAFT_06);

test("a counted initial set syncs once the reader is past its last announce", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceOk(PEER, 2).encode(w, Version.DRAFT_06));
	await send(announceStart("a"));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("a"), kind: "start" });
	expect(announced.synced.peek()).toBe(false);

	await send(announceStart("b"));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("b"), kind: "start" });
	await settle();
	// Still handling the last of them.
	expect(announced.synced.peek()).toBe(false);

	void announced.next();
	await settle();
	expect(announced.synced.peek()).toBe(true);

	announced.close();
	subscriber.close();
});

test("an empty initial set syncs on the answer", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.routes();
	await settle();
	expect(announced.synced.peek()).toBe(false);

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));
	await settle();
	expect(announced.synced.peek()).toBe(true);

	announced.close();
	subscriber.close();
});

test("a counted initial set cut short is a protocol violation", async () => {
	for (const cut of ["ended", "fin"] as const) {
		const { subscriber, send, finish, sessionEnded, settle } = announceHarness(Version.DRAFT_06);
		const announced = subscriber.routes();
		await settle();

		await send((w) => new AnnounceOk(PEER, 2).encode(w, Version.DRAFT_06));
		await send(announceStart("a"));
		if (cut === "ended") {
			await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_06));
		} else {
			finish();
		}

		expect(await announced.next()).toMatchObject({ prefix: Path.from("a"), kind: "start" });
		await expect(announced.next()).rejects.toThrow("inside the initial set");
		expect((await sessionEnded())?.closeCode).toBe(SessionCode.ProtocolViolation);
		subscriber.close();
	}
});

test.each([Version.DRAFT_03, Version.DRAFT_04])(
	"a version without a boundary syncs once the request is out on %s",
	async (version) => {
		const yielding = spyOn(Budget.prototype, "poll").mockImplementation(() => yieldNow());
		const { subscriber, settle } = announceHarness(version);
		const announced = subscriber.routes();
		try {
			expect(announced.synced.peek()).toBe(false);
			await settle();
			expect(announced.synced.peek()).toBe(true);
		} finally {
			announced.close();
			subscriber.close();
			yielding.mockRestore();
		}
	},
);

test("announce init is the initial set", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_02);
	const announced = subscriber.routes();
	await settle();

	await send((w) => new AnnounceInit([Path.from("room")]).encode(w, Version.DRAFT_02));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });
	void announced.next();
	await settle();
	expect(announced.synced.peek()).toBe(true);

	announced.close();
	subscriber.close();
});

// A FETCH waits for its answer, or a slot a transport queued it for, until its caller leaves.
test.each(["answer", "credit"] as const)("FETCH waiting for %s ends with its caller", async (stage) => {
	const clock = new Manual();
	const installed = clock.install();
	const { quic, streams, opened } = fakeSession(stage === "credit" ? [1] : []);
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	const cancel = new AbortController();
	let failure: unknown;
	const fetch = subscriber
		.fetchGroup(Path.from("room"), "video", 0n, { signal: cancel.signal })
		.catch((err: unknown) => {
			failure = err;
		});
	try {
		await opened(0);
		await streams[0].reading;
		await answerTrackInfo(streams[0], Version.DRAFT_07);
		await opened(1);
		if (stage === "answer") await streams[1].reading;
		await clock.advance(Duration.millis(3_600_000));
		expect(failure).toBeUndefined();
		const reason = new Error("left");
		cancel.abort(reason);
		await fetch;
		expect(failure).toBe(reason);
		// Credit granted once the caller has left can't send the FETCH.
		await clock.advance(Duration.ZERO);
		if (stage === "credit") streams[1].release();
		if (stage === "credit") {
			await drainReactions();
			expect(streams[1].written).toEqual([]);
		} else {
			await streams[1].aborted;
		}
	} finally {
		subscriber.close();
		installed.close();
	}
});

/**
 * A PROBE stream to a peer whose SETUP advertised `level`: `sent` reads back what the
 * subscriber wrote, and `finish` plays the peer's FIN.
 */
function probeTargetHarness(level: ProbeLevel, options?: { failTarget?: boolean; stallTarget?: boolean }) {
	const version = Version.DRAFT_06;
	let written!: ReadableStreamDefaultController<Uint8Array>;
	const sent = new Reader(
		new ReadableStream<Uint8Array>({ start: (controller) => (written = controller) }),
		undefined,
		version,
	);
	let inbound!: ReadableStreamDefaultController<Uint8Array>;
	let writes = 0;
	let stall!: () => void;
	const stalled = new Promise<void>((resolve) => (stall = resolve));
	let release!: () => void;
	const released = new Promise<void>((resolve) => (release = resolve));
	let reset!: (reason: unknown) => void;
	const aborted = new Promise<unknown>((resolve) => (reset = resolve));
	const quic = {
		createBidirectionalStream: async () => ({
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (inbound = controller) }),
			writable: new WritableStream<Uint8Array>({
				write: async (chunk, controller) => {
					// Only the stream type precedes the first target.
					if (writes++ > 0) {
						if (options?.failTarget) throw new Error("write failed");
						// A peer that stops draining the stream at the first target: only `release`
						// or a reset ends that write.
						if (options?.stallTarget && writes === 2) {
							stall();
							await new Promise<void>((resolve, reject) => {
								void released.then(resolve);
								controller.signal.addEventListener("abort", () => {
									reset(controller.signal.reason);
									reject(controller.signal.reason);
								});
							});
						}
					}
					written.enqueue(new Uint8Array(chunk));
				},
				close: () => written.close(),
			}),
		}),
	} as unknown as WebTransport;
	const peerSetup = new Signal<Setup | undefined>(new Setup({ probe: level }));
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n), new Signal<ProbeStats>({}), peerSetup);
	return {
		subscriber,
		sent,
		version,
		stalled,
		release,
		aborted,
		finish: () => inbound.close(),
	};
}

// The viewers' sum reaches the peer as its PROBE target: only once nonzero, on every change,
// and retracted with a zero, while an opt-out sends nothing at all.
test("the PROBE stream sends the viewers' target to an Increase peer", async () => {
	const clock = new Manual();
	const installed = clock.install();
	using _clock = { [Symbol.dispose]: () => installed.close() };
	const { subscriber, sent, version, finish } = probeTargetHarness(ProbeLevel.Increase);
	const viewers = new Viewers();
	viewers.setTarget(0);
	const running = subscriber.runProbe(undefined, viewers.target);
	expect(await sent.u53()).toBe(StreamId.Probe);

	// Opted out: a viewer moves nothing onto the wire until the override clears.
	const first = viewers.watch(1_000_000);
	viewers.setTarget(undefined);
	expect((await Probe.decode(sent, version)).bitrate).toBe(1_000_000);

	const second = viewers.watch(2_000_000);
	expect((await Probe.decode(sent, version)).bitrate).toBe(3_000_000);

	first.close();
	expect((await Probe.decode(sent, version)).bitrate).toBe(2_000_000);

	// Dropping to zero retracts the target as unknown on the wire.
	second.close();
	expect((await Probe.decode(sent, version)).bitrate).toBeUndefined();

	// Reading the target back only proves its bytes were written: the writer may still be
	// yielding its write budget, and a FIN then would reset the stream instead of closing it.
	await clock.advance(Duration.ZERO);
	finish();
	await running;
	expect(await sent.done()).toBe(true);
	subscriber.close();
	viewers.close();
});

// The writer and reader are independent: a failed target write ends the probe even while
// the peer keeps its side open.
test("a failed PROBE target write ends the probe", async () => {
	const { subscriber, sent } = probeTargetHarness(ProbeLevel.Increase, { failTarget: true });
	const viewers = new Viewers();
	const viewer = viewers.watch(1_000_000);
	const running = subscriber.runProbe(undefined, viewers.target);
	expect(await sent.u53()).toBe(StreamId.Probe);

	await running;
	viewer.close();
	subscriber.close();
	viewers.close();
});

// Ending the probe resets a stream whose target write the peer never drained, rather than
// queueing a close behind that write and holding the stream open.
test("ending the probe resets a stalled PROBE target write", async () => {
	const { subscriber, sent, stalled, aborted } = probeTargetHarness(ProbeLevel.Increase, { stallTarget: true });
	const viewers = new Viewers();
	const viewer = viewers.watch(1_000_000);
	const stop = new AbortController();
	const running = subscriber.runProbe(stop.signal, viewers.target);
	expect(await sent.u53()).toBe(StreamId.Probe);
	await stalled;

	stop.abort();
	await running;
	expect(await aborted).toBeDefined();
	viewer.close();
	subscriber.close();
	viewers.close();
});

// A peer that stops reading holds one target: the changes made meanwhile queue nothing
// behind it, and only the latest is sent once the write drains.
test("a stalled PROBE writer sends only the latest target once it drains", async () => {
	const clock = new Manual();
	const installed = clock.install();
	using _clock = { [Symbol.dispose]: () => installed.close() };
	const { subscriber, sent, version, stalled, release, finish } = probeTargetHarness(ProbeLevel.Increase, {
		stallTarget: true,
	});
	const viewers = new Viewers();
	const viewer = viewers.watch(1_000_000);
	const running = subscriber.runProbe(undefined, viewers.target);
	expect(await sent.u53()).toBe(StreamId.Probe);
	await stalled;

	for (const rate of [2_000_000, 3_000_000, 4_000_000]) {
		viewer.update(rate);
		await drainReactions();
	}
	release();
	expect((await Probe.decode(sent, version)).bitrate).toBe(1_000_000);
	expect((await Probe.decode(sent, version)).bitrate).toBe(4_000_000);

	// Let the last write finish yielding before the FIN, as in the test above.
	await clock.advance(Duration.ZERO);
	finish();
	await running;
	expect(await sent.done()).toBe(true);
	viewer.close();
	subscriber.close();
	viewers.close();
});

// A Report-level peer only measures, so it is never sent a target to ignore.
test("the PROBE stream sends no target to a Report peer", async () => {
	const { subscriber, sent, finish } = probeTargetHarness(ProbeLevel.Report);
	const viewers = new Viewers();
	const viewer = viewers.watch(1_000_000);
	const running = subscriber.runProbe(undefined, viewers.target);
	expect(await sent.u53()).toBe(StreamId.Probe);

	finish();
	await running;
	expect(await sent.done()).toBe(true);
	viewer.close();
	subscriber.close();
	viewers.close();
});

// Announce streams overlapping at a path share the instance minted for it, but a replacement
// delivered on both, interleaved, is a new stream on each: holder counts alone would hand it
// the old instance, splicing the two.
test("an interleaved replacement mints a fresh instance", () => {
	const minted = new Minted();
	const path = Path.from("room/host");
	const old = minted.acquire(path);
	expect(Instance.isMinted(old)).toBe(true);
	expect(minted.acquire(path)).toBe(old);

	// END1, START1, END2, START2.
	minted.release(path, old, true);
	const replacement = minted.acquire(path);
	expect(replacement).not.toBe(old);
	minted.release(path, old, true);
	expect(minted.acquire(path)).toBe(replacement);

	// A stream closing ends nothing: the other's advertisement keeps its instance.
	minted.release(path, replacement, false);
	expect(minted.acquire(path)).toBe(replacement);
});

// Reader demand applies inside a frame too, including when its peer never sends another byte.
test("a FETCH stops a stalled frame only after its last coalesced reader leaves", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	try {
		const a = subscriber.fetchGroup(Path.from("room"), "video", 0n);
		const b = subscriber.fetchGroup(Path.from("room"), "video", 0n);
		await opened(0);
		await answerTrackInfo(streams[0]);
		await opened(1);
		await streams[1].reading;
		streams[1].inbound.enqueue(new Uint8Array([0, ...Varint.encode(SEGMENT_BYTES * 2)]));
		const first = await a;
		const last = await b;
		const firstFrame = await first.nextFrame();
		const lastFrame = await last.nextFrame();
		if (!firstFrame || !lastFrame) throw new Error("missing frame header");
		first.close();
		streams[1].inbound.enqueue(Uint8Array.of(7));
		expect(await lastFrame.readChunk()).toEqual(Uint8Array.of(7));
		last.close();
		const err = fromTransport(await streams[1].aborted) as StreamError;
		expect(err.code).toBe(StreamCode.Cancel);
	} finally {
		subscriber.close();
	}
});

test("a cancelled legacy datagram lookup cannot refuse a stream answer arriving before auth settles", async () => {
	const { quic, streams, opened } = fakeSession();
	Object.defineProperty(quic, "datagrams", {
		value: { maxDatagramSize: 1200, readable: new ReadableStream<Uint8Array>() },
	});
	const waiting = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	let calls = 0;
	const auth = {
		async ready() {
			if (++calls === 2) {
				waiting.resolve();
				await release.promise;
			}
			return true;
		},
	} as unknown as AuthSession;
	const version = Version.DRAFT_06;
	const subscriber = new Subscriber(
		quic,
		version,
		HopSchema.parse(1n),
		undefined,
		undefined,
		undefined,
		undefined,
		undefined,
		auth,
	);
	const remote = subscriber.consume(Path.from("room"));
	const grouped = remote.track("video").subscribe();
	let datagrams: ReturnType<typeof remote.datagrams> | undefined;
	try {
		await opened(1);
		datagrams = remote.datagrams("video");
		await waiting.promise;
		const info = datagrams.info();
		datagrams.close();
		await expect(info).rejects.toBeInstanceOf(StreamError);
		await answerTrackInfo(streams[0], version);
		await grouped.info();
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => streams[1].inbound.enqueue(new Uint8Array(chunk)) }),
			version,
		);
		await encodeSubscribeResponse(writer, { start: new SubscribeStart([0n]) }, version);
		await encodeSubscribeResponse(writer, { finish: new SubscribeFinish(0n) }, version);
		streams[1].inbound.close();
		expect(await grouped.recvGroup()).toBeUndefined();
	} finally {
		release.resolve();
		datagrams?.close();
		grouped.close();
		remote.close();
		subscriber.close();
	}
});

// A lite-07 peer's announce stream, after the request: what the peer writes, and the replies
// the subscriber sends back. `another` opens a further one on the session as its `index`th stream.
async function routedStream(options?: RouteOptions, getStats?: () => Promise<unknown>) {
	const version = Version.DRAFT_07;
	const { quic, opened } = fakeSession();
	if (getStats) Object.assign(quic, { getStats });
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const another = async (index: number, scope?: Path.Pattern) => {
		const announced = subscriber.routes(scope, options);
		const stream = await opened(index);
		const peer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => stream.inbound.enqueue(new Uint8Array(chunk)) }),
			version,
		);
		const replies = new Reader(stream.outbound, undefined, version);
		expect(await replies.u53()).toBe(StreamId.Announce);
		await AnnounceRequest.decode(replies, version);
		await new AnnounceOk(PEER, 0).encode(peer, version);
		const send = (msg: AnnounceBroadcast) => encodeAnnounceBroadcast(peer, msg, version);
		// Bytes as a newer or broken peer might write them.
		const raw = (bytes: number[]) => stream.inbound.enqueue(new Uint8Array(bytes));
		return { announced, send, raw, replies };
	};
	const { announced, send, raw, replies } = await another(0);
	// The TRACK request a consume of `path` opens as its `index`th stream.
	const trackRequest = async (path: Path.Valid, index: number, pin?: Instance.Valid) => {
		const consumed = subscriber.consume(path, pin);
		const info = consumed
			.track("video")
			.info()
			.catch(() => {});
		const opening = await opened(index);
		const reader = new Reader(opening.outbound, undefined, version);
		expect(await reader.u53()).toBe(StreamId.Track);
		const request = await Track.decode(reader, version);
		return { request, consumed, info };
	};
	return { subscriber, announced, send, raw, replies, version, trackRequest, another, opened };
}

// A route or claim whose forwarded content this subscriber refuses is withdrawn alone: the session,
// the peer's other route, and its claims stay, and the ids after it line up. Mirrors rs/moq-net.
test("a lite-07 refused route or claim withdraws only itself", async () => {
	const { subscriber, announced, send, raw } = await routedStream();
	const [a, b, c, d] = ["live/a", "live/b", "live/c", "live/d"].map((path) => Path.from(path));
	const instance = Instance.parse("i");

	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: a, route: 0n, instance });
	await send({ status: "active", suffix: b, route: 1n, instance });
	await send({ status: "floor", id: 0n, revision: 0n });
	await send({ status: "floor", id: 1n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: a, kind: "start" });
	expect(await announced.next()).toMatchObject({ prefix: b, kind: "start" });

	// ROUTE_UPDATE of Route ID 0 with a flag no version defines, and the field it might announce.
	raw([7, 5, 0, 1, 0, 0x80, 42]);
	expect(await announced.next()).toMatchObject({ prefix: a, kind: "offline" });

	// Announce ID 2: an ANNOUNCE_START under Route ID 1 whose path has an empty segment.
	raw([0, 13, 0, 0, 4, ...new TextEncoder().encode("x//y"), 1, 1, 0x69, 0, 0, 0]);
	await send({ status: "endedId", id: 2n });
	await send({ status: "active", suffix: c, route: 1n, instance });
	await send({ status: "floor", id: 1n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: c, kind: "start" });

	// Forwarded usably again, the route's claims come back.
	await send({ status: "routeUpdate", id: 0n, advert: ADVERT });
	expect(await announced.next()).toMatchObject({ prefix: a, kind: "start" });

	// ROUTE_START to the reserved node takes Route ID 2, and its claims never show.
	raw([6, 5, 0, 1, 0, 0, 0]);
	await send({ status: "active", suffix: d, route: 2n, instance });
	await send({ status: "floor", id: 2n, revision: 0n });
	await send({ status: "endedId", id: 3n });
	await send({ status: "floor", id: 1n, revision: 2n });
	expect(await announced.next()).toMatchObject({ prefix: c, kind: "end" });

	announced.close();
	subscriber.close();
});

// A claim this subscriber refuses was not withdrawn by its origin: it goes offline, and while its
// route holds one, the route's floors certify nothing, so another claim's end there goes offline
// too. Mirrors rs/moq-net.
test("a lite-07 refused claim goes offline, its route certifying nothing meanwhile", async () => {
	const { subscriber, announced, send, raw } = await routedStream();
	const [a, b, e] = ["live/a", "live/b", "live/e"].map((path) => Path.from(path));
	const instance = Instance.parse("i");

	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: a, route: 0n, instance });
	await send({ status: "active", suffix: b, route: 0n, instance });
	await send({ status: "floor", id: 0n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: a, kind: "start" });
	expect(await announced.next()).toMatchObject({ prefix: b, kind: "start" });

	// ANNOUNCE_RESTART of Announce ID 0 to an empty instance. JS reads no Lite-06 Chain, so an
	// ANNOUNCE_UPDATE has nothing it could refuse.
	const refuse = () => raw([3, 5, 0, 0, 0, 0, 0]);
	refuse();
	await send({ status: "floor", id: 0n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: a, kind: "offline" });
	await send({ status: "endedId", id: 1n });
	await send({ status: "floor", id: 0n, revision: 2n });
	expect(await announced.next()).toMatchObject({ prefix: b, kind: "offline" });

	// A usable restart brings it back, and refusing the next takes it offline again.
	await send({ status: "restart", id: 0n, instance: Instance.parse("j") });
	await send({ status: "floor", id: 0n, revision: 3n });
	expect(await announced.next()).toMatchObject({ prefix: a, kind: "start" });
	refuse();
	await send({ status: "floor", id: 0n, revision: 4n });
	expect(await announced.next()).toMatchObject({ prefix: a, kind: "offline" });

	// Once it ends, the route's floors certify again.
	await send({ status: "endedId", id: 0n });
	await send({ status: "active", suffix: e, route: 0n, instance });
	await send({ status: "floor", id: 0n, revision: 5n });
	expect(await announced.next()).toMatchObject({ prefix: e, kind: "start" });
	await send({ status: "endedId", id: 2n });
	await send({ status: "floor", id: 0n, revision: 6n });
	expect(await announced.next()).toMatchObject({ prefix: e, kind: "end" });

	announced.close();
	subscriber.close();
});

// A claim at the session root has the empty path, which is a path all the same: its updates,
// restarts, and end apply as any other's.
test("a lite-07 claim at the root updates and ends", async () => {
	const { subscriber, announced, send } = await routedStream();
	const root = Path.empty();

	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: root, route: 0n, instance: Instance.parse("i") });
	await send({ status: "floor", id: 0n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: root, kind: "start" });

	await send({ status: "restart", id: 0n, instance: Instance.parse("j") });
	await send({ status: "floor", id: 0n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: root, kind: "restart" });

	await send({ status: "endedId", id: 0n });
	await send({ status: "floor", id: 0n, revision: 2n });
	expect(await announced.next()).toMatchObject({ prefix: root, kind: "end" });

	// Its path is free again, so starting it anew is no duplicate.
	await send({ status: "active", suffix: root, route: 0n, instance: Instance.parse("k") });
	await send({ status: "floor", id: 0n, revision: 3n });
	expect(await announced.next()).toMatchObject({ prefix: root, kind: "start" });

	announced.close();
	subscriber.close();
});

// A route this subscriber refused is no view of its node: a newer floor on it hides nothing
// another stream carries, as rs/moq-net's node never holds the route.
test("a lite-07 refused route's floors hide no other stream's claims", async () => {
	const { subscriber, announced, send, another } = await routedStream();
	const live = await another(1, Path.Pattern.subtree(Path.from("live")));
	const instance = Instance.parse("i");
	// One varint byte, so the raw message below spells it.
	const node = 7n;

	await send({ status: "routeStart", node, advert: ADVERT });
	await send({ status: "active", suffix: Path.from("live/x"), route: 0n, instance });
	await send({ status: "floor", id: 0n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: Path.from("live/x"), kind: "start" });

	// ROUTE_START to the node with a flag no version defines, then a newer floor without live/x.
	live.raw([6, 5, 7, 1, 0, 0x80, 42]);
	await live.send({ status: "floor", id: 0n, revision: 5n });
	// A claim of another node on that stream shows once it read all of the above.
	await live.send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await live.send({ status: "active", suffix: Path.from("z"), route: 1n, instance });
	await live.send({ status: "floor", id: 1n, revision: 0n });
	expect(await live.announced.next()).toMatchObject({ prefix: Path.from("live/z"), kind: "start" });

	// What the first stream shows next is its own new claim, not live/x withdrawn.
	await send({ status: "active", suffix: Path.from("live/y"), route: 0n, instance });
	await send({ status: "floor", id: 0n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: Path.from("live/y"), kind: "start" });

	live.announced.close();
	announced.close();
	subscriber.close();
});

// A route refused once live leaves what its last floor certified, as an ended one does: the claims
// it reads afterwards never rewrite that.
test("a lite-07 route refused once live leaves its last floor as it was", async () => {
	const { subscriber, announced, send, raw, another } = await routedStream();
	const live = await another(1, Path.Pattern.subtree(Path.from("live")));
	const instance = Instance.parse("i");
	const node = 7n;
	const x = Path.from("live/x");

	// Both streams carry live/x, the first at revision 5 and the second lagging at 3.
	await send({ status: "routeStart", node, advert: ADVERT });
	await send({ status: "active", suffix: x, route: 0n, instance });
	await send({ status: "floor", id: 0n, revision: 5n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "start" });
	await live.send({ status: "routeStart", node, advert: ADVERT });
	await live.send({ status: "active", suffix: Path.from("x"), route: 0n, instance });
	await live.send({ status: "floor", id: 0n, revision: 3n });
	expect(await live.announced.next()).toMatchObject({ prefix: x, kind: "start" });

	// The first stream's route is refused, then ends live/x at a floor no view takes.
	raw([7, 5, 0, 1, 0, 0x80, 42]);
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "offline" });
	await send({ status: "endedId", id: 0n });
	await send({ status: "floor", id: 0n, revision: 6n });

	// The second stream's next floor still has live/x, as revision 5 did: what it shows next is
	// another node's claim, not live/x withdrawn.
	await live.send({ status: "floor", id: 0n, revision: 4n });
	await live.send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await live.send({ status: "active", suffix: Path.from("z"), route: 1n, instance });
	await live.send({ status: "floor", id: 1n, revision: 0n });
	expect(await live.announced.next()).toMatchObject({ prefix: Path.from("live/z"), kind: "start" });

	live.announced.close();
	announced.close();
	subscriber.close();
});

// Two streams carry one origin's claims, and a withdrawal reaches one first. Of the floors that
// certify a claim, the newest decides whether it exists, so the lagging stream's copy is held back
// rather than selected and refused: also once the newer stream's route ends.
test("a lite-07 claim another stream's newer floor withdrew is not selected", async () => {
	const { subscriber, announced, send, trackRequest, another } = await routedStream();
	const live = await another(1, Path.Pattern.subtree(Path.from("live")));
	const x = Path.from("live/x");
	const a = Instance.parse("a");
	const b = Instance.parse("b");
	const y = Instance.parse("y");

	// NODE_A claims live/x and live/y, cheaply, on both streams at revision 0.
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: x, route: 0n, instance: a });
	await send({ status: "active", suffix: Path.from("live/y"), route: 0n, instance: y });
	await send({ status: "floor", id: 0n, revision: 0n });
	await live.send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await live.send({ status: "active", suffix: Path.from("x"), route: 0n, instance: a });
	await live.send({ status: "active", suffix: Path.from("y"), route: 0n, instance: y });
	await live.send({ status: "floor", id: 0n, revision: 0n });
	expect(await live.announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_A } });
	expect(await live.announced.next()).toMatchObject({ prefix: Path.from("live/y"), kind: "start" });
	// A dearer NODE_B claims live/x on the first stream only.
	await send({ status: "routeStart", node: NODE_B, advert: { ...ADVERT, metric: 5n } });
	await send({ status: "active", suffix: x, route: 1n, instance: b });
	await send({ status: "floor", id: 1n, revision: 0n });

	// NODE_A withdraws live/x at revision 1, which only the first stream has delivered.
	await send({ status: "endedId", id: 0n });
	await send({ status: "floor", id: 0n, revision: 1n });
	// The lagging stream ends its copy, and keeps the claim it still agrees on.
	expect(await live.announced.next()).toMatchObject({ prefix: x, kind: "end", route: { node: NODE_A } });
	const first = await trackRequest(x, 2);
	expect(first.request).toMatchObject({ instance: b, node: NODE_B });

	// The first stream lets its route to NODE_A go, and a third stream, as far behind, then lands
	// its copy: what the ended route's floor certified still holds, so a fresh pick of live/x
	// still passes over the cheaper copy.
	const exact = await another(3, Path.Pattern.subtree(x));
	await send({ status: "routeEnd", id: 0n });
	await exact.send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await exact.send({ status: "active", suffix: Path.empty(), route: 0n, instance: a });
	await exact.send({ status: "floor", id: 0n, revision: 0n });
	await exact.send({ status: "routeStart", node: NODE_B, advert: { ...ADVERT, metric: 5n } });
	await exact.send({ status: "active", suffix: Path.empty(), route: 1n, instance: b });
	await exact.send({ status: "floor", id: 1n, revision: 0n });
	expect(await exact.announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_B } });
	const second = await trackRequest(x, 4);
	expect(second.request).toMatchObject({ instance: b, node: NODE_B });

	// The lagging stream catches up and reports nothing more for live/x.
	await live.send({ status: "endedId", id: 0n });
	await live.send({ status: "floor", id: 0n, revision: 1n });
	await live.send({ status: "active", suffix: Path.from("z"), route: 0n, instance: y });
	await live.send({ status: "floor", id: 0n, revision: 2n });
	expect(await live.announced.next()).toMatchObject({ prefix: Path.from("live/z"), kind: "start" });

	first.consumed.close();
	second.consumed.close();
	exact.announced.close();
	live.announced.close();
	announced.close();
	subscriber.close();
	await Promise.all([first.info, second.info]);
});

// A floor that certifies nothing withdraws no knowledge: the revision its route's last floor
// certified still decides, so the lagging stream's withdrawn copy stays held back.
test("a lite-07 floor that certifies nothing keeps the withdrawal its route certified", async () => {
	const { subscriber, announced, send, trackRequest, another } = await routedStream();
	const live = await another(1, Path.Pattern.subtree(Path.from("live")));
	const x = Path.from("live/x");
	const a = Instance.parse("a");
	const b = Instance.parse("b");

	// NODE_A claims live/x cheaply on both streams at revision 0; a dearer NODE_B on the first.
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: x, route: 0n, instance: a });
	await send({ status: "floor", id: 0n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_A } });
	await live.send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await live.send({ status: "active", suffix: Path.from("x"), route: 0n, instance: a });
	await live.send({ status: "floor", id: 0n, revision: 0n });
	expect(await live.announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_A } });
	await send({ status: "routeStart", node: NODE_B, advert: { ...ADVERT, metric: 5n } });
	await send({ status: "active", suffix: x, route: 1n, instance: b });
	await send({ status: "floor", id: 1n, revision: 0n });

	// NODE_A withdraws live/x at revision 1 on the first stream, whose next floor certifies nothing.
	await send({ status: "endedId", id: 0n });
	await send({ status: "floor", id: 0n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "restart", route: { node: NODE_B } });
	expect(await live.announced.next()).toMatchObject({ prefix: x, kind: "end", route: { node: NODE_A } });
	await send({ status: "floor", id: 0n });
	// Another node's claim marks the uncertified floor as applied.
	const w = Path.from("live/w");
	await send({ status: "active", suffix: w, route: 1n, instance: b });
	await send({ status: "floor", id: 1n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: w, kind: "start" });

	const pick = await trackRequest(x, 2);
	expect(pick.request).toMatchObject({ instance: b, node: NODE_B });

	pick.consumed.close();
	live.announced.close();
	announced.close();
	subscriber.close();
	await pick.info;
});

// A kept floor decides only while a live one certifies the path: once the last certifying route
// ends, a claim another stream carries uncertified shows again.
test("a lite-07 uncertified claim shows once its last certifier ends", async () => {
	const { subscriber, announced, send, another } = await routedStream();
	const live = await another(1, Path.Pattern.subtree(Path.from("live")));
	const x = Path.from("live/x");
	const y = Path.from("live/y");
	const a = Instance.parse("a");

	// The first stream certifies NODE_A's claims at revision 1, without live/x.
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: y, route: 0n, instance: a });
	await send({ status: "floor", id: 0n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: y, kind: "start" });
	// The second carries live/x under a floor that certifies nothing: held back.
	await live.send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await live.send({ status: "active", suffix: Path.from("x"), route: 0n, instance: a });
	await live.send({ status: "active", suffix: Path.from("y"), route: 0n, instance: a });
	await live.send({ status: "floor", id: 0n });
	expect(await live.announced.next()).toMatchObject({ prefix: y, kind: "start" });

	await send({ status: "routeEnd", id: 0n });
	expect(await live.announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_A } });

	live.announced.close();
	announced.close();
	subscriber.close();
});

// A route's kept floor that a later one prunes re-shows the claims it held back, though the
// pruning floor carries no batch.
test("a lite-07 claim a pruned floor held back shows without a batch", async () => {
	const { subscriber, announced, send } = await routedStream();
	const x = Path.from("live/x");
	const y = Path.from("live/y");
	const w = Path.from("live/w");
	const a = Instance.parse("a");

	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: y, route: 0n, instance: a });
	await send({ status: "floor", id: 0n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: y, kind: "start" });
	// An older revision adds live/x: the kept revision 1 holds it back.
	await send({ status: "active", suffix: x, route: 0n, instance: a });
	await send({ status: "floor", id: 0n, revision: 0n });
	// A floor that certifies nothing leaves no floor to decide live/x, so it shows.
	await send({ status: "floor", id: 0n });
	// Another node's claim marks the floors as applied.
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: w, route: 1n, instance: a });
	await send({ status: "floor", id: 1n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_A } });

	announced.close();
	subscriber.close();
});

// A claim above a newer floor's region is older knowledge there: a consume below that region does
// not select it, as Rust's `covering` presents no claim above the newest certifier.
test("a lite-07 consume skips a claim above a newer floor's region", async () => {
	const { subscriber, announced, send, trackRequest, another } = await routedStream();
	const x = await another(1, Path.Pattern.subtree(Path.from("live/x")));
	const root = Path.from("live");
	const a = Instance.parse("a");

	// NODE_A claims live at revision 0; a dearer NODE_B claims it too.
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: root, route: 0n, instance: a });
	await send({ status: "floor", id: 0n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: root, kind: "start", route: { node: NODE_A } });
	await send({ status: "routeStart", node: NODE_B, advert: { ...ADVERT, metric: 5n } });
	await send({ status: "active", suffix: root, route: 1n, instance: a });
	await send({ status: "floor", id: 1n, revision: 0n });
	// NODE_A's revision 1 presents only live/x/m under live/x: it withdrew live.
	await x.send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await x.send({ status: "active", suffix: Path.from("m"), route: 0n, instance: a });
	await x.send({ status: "floor", id: 0n, revision: 1n });
	expect(await x.announced.next()).toMatchObject({ prefix: Path.from("live/x/m"), kind: "start" });

	const pick = await trackRequest(Path.from("live/x"), 2);
	expect(pick.request).toMatchObject({ instance: a, node: NODE_B });

	pick.consumed.close();
	x.announced.close();
	announced.close();
	subscriber.close();
	await pick.info;
});

// A route refused once live stops covering, as an ended one does: a held consume a narrower floor
// of it took off a broader claim is routable again once that floor is gone.
test("a lite-07 route refused once live gives a held consume back its broader claim", async () => {
	const { subscriber, announced, send, trackRequest, another, opened } = await routedStream();
	const live = Path.from("live");
	const y = Path.from("live/x/y");
	const minted = Mint.fresh();

	// NODE_A claims live under a floor that certifies nothing, and a consume of live/x/y takes it.
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: live, route: 0n, instance: minted });
	await send({ status: "floor", id: 0n });
	expect(await announced.next()).toMatchObject({ prefix: live, kind: "start" });
	const x = await another(1, Path.Pattern.subtree(Path.from("live/x")));
	const held = await trackRequest(y, 2);
	expect(held.request).toMatchObject({ node: NODE_A });

	// NODE_A's revision 1 presents only live/x/m under live/x, so live no longer covers live/x/y.
	await x.send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await x.send({ status: "active", suffix: Path.from("m"), route: 0n, instance: minted });
	await x.send({ status: "floor", id: 0n, revision: 1n });
	expect(await x.announced.next()).toMatchObject({ prefix: Path.from("live/x/m"), kind: "start" });
	await held.info;

	// That route is refused: nothing certifies live/x/y any more, so live covers it again.
	x.raw([7, 5, 0, 1, 0, 0x80, 42]);
	expect(await x.announced.next()).toMatchObject({ prefix: Path.from("live/x/m"), kind: "offline" });
	const info = held.consumed
		.track("audio")
		.info()
		.then(
			() => "answered",
			() => "refused",
		);
	expect(await Promise.race([opened(3).then(() => "opened"), info])).toBe("opened");

	held.consumed.close();
	x.announced.close();
	announced.close();
	subscriber.close();
	await info;
});

// A route that cannot be used for a while keeps its claims' place in a tie, so the announcements
// and the consumes still agree once it recovers, as a Rust entry keeps its id.
test("a lite-07 tie keeps its winner across a route's unusable spell", async () => {
	const { subscriber, announced, send, trackRequest } = await routedStream();
	const x = Path.from("live/x");
	const a = Instance.parse("a");

	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: x, route: 0n, instance: a });
	await send({ status: "floor", id: 0n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_A } });
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: x, route: 1n, instance: a });
	await send({ status: "floor", id: 1n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "update", route: { node: NODE_B } });
	// NODE_A's route crosses this session, then recovers.
	await send({ status: "routeUpdate", id: 0n, advert: { ...ADVERT, hops: [subscriber.hop] } });
	await send({ status: "routeUpdate", id: 0n, advert: ADVERT });
	// Another claim marks the updates as applied.
	const w = Path.from("live/w");
	await send({ status: "active", suffix: w, route: 1n, instance: a });
	await send({ status: "floor", id: 1n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: w, kind: "start" });

	const pick = await trackRequest(x, 1);
	expect(pick.request).toMatchObject({ instance: a, node: NODE_B });

	pick.consumed.close();
	announced.close();
	subscriber.close();
	await pick.info;
});

// Lite-07 carries a minted instance, as a relay forwards a legacy publisher's claim, so a consume
// pinned to one asks for it rather than for whichever instance wins the path.
test("a lite-07 consume pinned to a minted instance asks for it", async () => {
	const { subscriber, announced, send, trackRequest } = await routedStream();
	const x = Path.from("live/x");
	const a = Instance.parse("a");
	const minted = Mint.fresh();

	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: x, route: 0n, instance: a });
	await send({ status: "floor", id: 0n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_A } });
	// A dearer route carries the minted instance.
	await send({ status: "routeStart", node: NODE_B, advert: { ...ADVERT, metric: 5n } });
	await send({ status: "active", suffix: x, route: 1n, instance: minted });
	await send({ status: "floor", id: 1n, revision: 0n });
	// Another claim marks the floor as applied.
	const w = Path.from("live/w");
	await send({ status: "active", suffix: w, route: 1n, instance: a });
	await send({ status: "floor", id: 1n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: w, kind: "start" });

	const pick = await trackRequest(x, 1, minted);
	expect(pick.request).toMatchObject({ instance: minted, node: NODE_B });

	pick.consumed.close();
	announced.close();
	subscriber.close();
	await pick.info;
});

// A lagging stream's copy of a claim takes the cost the newest certifying floor gives it, so a
// re-priced origin does not keep winning on the cost it had.
test("a lite-07 lagging copy carries the cost a newer floor certified", async () => {
	const { subscriber, announced, send, trackRequest, another } = await routedStream();
	const live = await another(1, Path.Pattern.subtree(Path.from("live")));
	const x = Path.from("live/x");
	const a = Instance.parse("a");
	const b = Instance.parse("b");

	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: x, route: 0n, instance: a });
	await send({ status: "floor", id: 0n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "start", route: { node: NODE_A } });
	await live.send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await live.send({ status: "active", suffix: Path.from("x"), route: 0n, instance: a });
	await live.send({ status: "floor", id: 0n, revision: 0n });
	expect(await live.announced.next()).toMatchObject({ prefix: x, kind: "start", route: { cost: 1n } });
	await send({ status: "routeStart", node: NODE_B, advert: { ...ADVERT, metric: 5n } });
	await send({ status: "active", suffix: x, route: 1n, instance: b });
	await send({ status: "floor", id: 1n, revision: 0n });

	// NODE_A raises its cost to 100 at revision 1, which only the first stream has delivered.
	await send({ status: "update", id: 0n, cost: 100n });
	await send({ status: "floor", id: 0n, revision: 1n });
	expect(await announced.next()).toMatchObject({ prefix: x, kind: "restart", route: { node: NODE_B } });
	expect(await live.announced.next()).toMatchObject({ prefix: x, kind: "update", route: { cost: 101n } });

	const pick = await trackRequest(x, 2);
	expect(pick.request).toMatchObject({ instance: b, node: NODE_B });

	pick.consumed.close();
	live.announced.close();
	announced.close();
	subscriber.close();
	await pick.info;
});

test("a lite-07 consume names the node now serving its instance", async () => {
	const { subscriber, announced, send, trackRequest } = await routedStream();
	const room = Path.from("room");
	const a = Instance.parse("a");

	// Two origin nodes reach one instance; the cheaper serves.
	await send({ status: "routeStart", node: NODE_A, advert: { ...ADVERT, metric: 5n } });
	await send({ status: "active", suffix: room, route: 0n, instance: a });
	await send({ status: "floor", id: 0n });
	await announced.next();
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: room, route: 1n, instance: a });
	await send({ status: "floor", id: 1n });
	await announced.next();
	const first = await trackRequest(room, 1);
	expect(first.request).toMatchObject({ instance: a, node: NODE_B });

	// The cheaper one goes: the same instance's consume now names the other.
	await send({ status: "routeEnd", id: 1n });
	await announced.next();
	const second = await trackRequest(room, 2);
	expect(second.consumed.closed).toBe(first.consumed.closed);
	expect(second.request).toMatchObject({ instance: a, node: NODE_A });

	first.consumed.close();
	second.consumed.close();
	subscriber.close();
	await Promise.all([first.info, second.info]);
});

test("a lite-07 path ranks only an instance's newest epoch", async () => {
	const { subscriber, announced, send } = await routedStream();
	const room = Path.from("room");
	const a = Instance.parse("a");
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: room, route: 0n, instance: a, epoch: 0n });
	await send({ status: "floor", id: 0n });
	expect(await announced.next()).toMatchObject({ prefix: room, route: { epoch: 0n } });

	// Dearer, but newer: it takes over.
	await send({ status: "routeStart", node: NODE_B, advert: { ...ADVERT, metric: 9n } });
	await send({ status: "active", suffix: room, route: 1n, instance: a, epoch: 1n });
	await send({ status: "floor", id: 1n });
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "restart", route: { epoch: 1n, node: NODE_B } });

	announced.close();
	subscriber.close();
});

// Each consume ranks every route's claim at its covering prefix for its own path, so two equal
// origins of a prefix spread its children by rendezvous hash, whichever announced first.
test("lite-07 children of a prefix two origins claim spread by node", async () => {
	for (const order of [
		[NODE_A, NODE_B],
		[NODE_B, NODE_A],
	]) {
		const { subscriber, announced, send, trackRequest } = await routedStream();
		const pool = Path.from("pool");
		for (const [route, node] of order.entries()) {
			await send({ status: "routeStart", node, advert: ADVERT });
			await send({ status: "active", suffix: pool, route: BigInt(route), instance: Instance.parse(`i${node}`) });
			await send({ status: "floor", id: BigInt(route) });
			await announced.next();
		}
		const job = Path.from("pool/job");
		// Pinned to `lite07_ties_match_js` in rs/moq-net, which picks NODE_B too.
		expect(rendezvousHash(job, NODE_A)).toBe(0xc376be7bf47aebbfn);
		expect(rendezvousHash(job, NODE_B)).toBe(0x5813213b6508aaeen);
		const { request, consumed, info } = await trackRequest(job, 1);
		expect(request.node).toBe(NODE_B);
		consumed.close();
		subscriber.close();
		await info;
	}
});

// A full tie at a claim's own path goes to the newest claim, as `lite07_ties_match_js` in
// rs/moq-net pins: a re-price into the tie leaves it in place, a restart makes it the newest.
// The announcements and the consumes pick alike.
test("a lite-07 tie at a claim's path goes to the newest claim, a re-price keeping its place", async () => {
	const { subscriber, announced, send, trackRequest } = await routedStream();
	const room = Path.from("room");
	const other = Path.from("other");
	const a = Instance.parse("a");
	const b = Instance.parse("b");
	const c = Instance.parse("c");

	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: room, route: 0n, instance: a, cost: 4n });
	await send({ status: "floor", id: 0n });
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "start", route: { node: NODE_A } });
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: room, route: 1n, instance: b });
	await send({ status: "floor", id: 1n });
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "restart", route: { node: NODE_B } });

	// The older claim re-prices into a tie: the newer keeps the path, so nothing is reported.
	await send({ status: "update", id: 0n, cost: 0n });
	await send({ status: "floor", id: 0n });
	await send({ status: "active", suffix: other, route: 1n, instance: b });
	await send({ status: "floor", id: 1n });
	expect(await announced.next()).toMatchObject({ prefix: other, kind: "start" });
	const kept = await trackRequest(room, 1);
	expect(kept.request).toMatchObject({ instance: b, node: NODE_B });

	// A restart at the tie is the newest claim: both pick it.
	await send({ status: "restart", id: 0n, instance: c });
	await send({ status: "floor", id: 0n });
	expect(await announced.next()).toMatchObject({
		prefix: room,
		kind: "restart",
		route: { instance: c, node: NODE_A },
	});
	const restarted = await trackRequest(room, 2);
	expect(restarted.request).toMatchObject({ instance: c, node: NODE_A });

	kept.consumed.close();
	restarted.consumed.close();
	announced.close();
	subscriber.close();
	await Promise.all([kept.info, restarted.info]);
});

// A consume pinned to an instance takes that instance's route, whichever the hash ranks first.
test("a lite-07 consume pinned to an instance names its route's node", async () => {
	const { subscriber, announced, send, trackRequest } = await routedStream();
	const pool = Path.from("pool");
	const a = Instance.parse("a");
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: pool, route: 0n, instance: a });
	await send({ status: "floor", id: 0n });
	await announced.next();
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: pool, route: 1n, instance: Instance.parse("b") });
	await send({ status: "floor", id: 1n });
	await announced.next();
	// NODE_B wins `pool/job` unpinned; see the spread test above.
	const { request, consumed, info } = await trackRequest(Path.from("pool/job"), 1, a);
	expect(request).toMatchObject({ instance: a, node: NODE_A });
	consumed.close();
	announced.close();
	subscriber.close();
	await info;
});

// A route's RTT is the relay's sum plus this link's own min RTT, unknown while either is, and it
// moves no pick: it only updates the route reported.
test("a lite-07 route reports its rtt as the relay's sum plus this link's", async () => {
	// The link's first read answers only after the route's first batch is on the stream.
	const stats = Promise.withResolvers<{ smoothedRtt: number }>();
	const { subscriber, announced, send } = await routedStream(undefined, () => stats.promise);
	const x = Path.from("x");
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: x, route: 0n, instance: Instance.parse("a") });
	await send({ status: "floor", id: 0n, revision: 0n });
	const first = announced.next();
	stats.resolve({ smoothedRtt: 10.2 });
	// The claim waits for that read, so the path is first reported with the sum: a listing shows
	// a route when its path turns present, and not again for an RTT.
	let next = (await first) as { kind: string; route: WireRoute };
	expect(next).toMatchObject({ kind: "start", route: { node: NODE_A } });
	expect(next.route.rtt?.asMillis()).toBe(31);

	// The relay could not measure a link on the path: the sum is unknown.
	await send({ status: "routeUpdate", id: 0n, advert: { ...ADVERT, seqno: 2n, rtt: undefined } });
	next = (await announced.next()) as { kind: string; route: WireRoute };
	expect(next).toMatchObject({ kind: "update", route: { node: NODE_A } });
	expect(next.route.rtt).toBeUndefined();

	announced.close();
	subscriber.close();
});

// Forwarding code asks for each origin node's claim at a path as its own route.
test("a lite-07 stream reports each node's claim with claims", async () => {
	const { subscriber, announced, send } = await routedStream({ claims: true });
	const pool = Path.from("pool");
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: pool, route: 0n, instance: Instance.parse("a") });
	await send({ status: "floor", id: 0n });
	expect(await announced.next()).toMatchObject({ prefix: pool, kind: "start", route: { node: NODE_A } });
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: pool, route: 1n, instance: Instance.parse("b") });
	await send({ status: "floor", id: 1n });
	expect(await announced.next()).toMatchObject({ prefix: pool, kind: "start", route: { node: NODE_B } });

	// The route ending takes only its own claim.
	await send({ status: "routeEnd", id: 0n });
	expect(await announced.next()).toMatchObject({ prefix: pool, kind: "offline", route: { node: NODE_A } });
	await send({ status: "update", id: 1n, cost: 3n });
	await send({ status: "floor", id: 1n });
	expect(await announced.next()).toMatchObject({ prefix: pool, kind: "update", route: { node: NODE_B } });

	announced.close();
	subscriber.close();
});

test("a lite-07 batch applies at its floor, and a held route turns unavailable", async () => {
	const { subscriber, announced, send, replies, version } = await routedStream();
	const room = Path.from("room");
	const other = Path.from("other");
	const a = Instance.parse("a");

	// Nothing under a route shows before its floor, even while another route's batch lands.
	await send({ status: "routeStart", node: NODE_A, advert: ADVERT });
	await send({ status: "active", suffix: room, route: 0n, instance: a, cost: 2n });
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: other, route: 1n, instance: a });
	await send({ status: "floor", id: 1n, revision: 0n });
	expect(await announced.next()).toMatchObject({ prefix: other, kind: "start" });
	await send({ status: "synced" });
	await send({ status: "floor", id: 0n, revision: 0n });
	// The claim's cost, the route's metric, and this session's link.
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "start", route: { cost: 3n, node: NODE_A } });

	// A held route keeps its claims, unavailable, until an update makes it usable again.
	await send({ status: "routeHold", id: 0n });
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "update", route: { available: false } });
	await send({ status: "routeUpdate", id: 0n, advert: { ...ADVERT, metric: 5n } });
	expect(await announced.next()).toMatchObject({
		prefix: room,
		kind: "update",
		route: { available: true, cost: 8n },
	});
	// Nothing ranks by RTT, so a change to it alone reports nothing.
	await send({ status: "routeUpdate", id: 0n, advert: { ...ADVERT, metric: 5n, rtt: 90n } });
	// Nor by hops, until the route crossed as many nodes as one may, or crossed us: then it is no
	// route at all, until it is usable again.
	await send({ status: "routeUpdate", id: 0n, advert: { ...ADVERT, metric: 5n, hops: [NODE_A, 3n] } });
	const full = Array.from({ length: ROUTE_MAX_HOPS }, (_, i) => BigInt(i + 1));
	for (const hops of [full, [NODE_A, subscriber.hop]]) {
		await send({ status: "routeUpdate", id: 0n, advert: { ...ADVERT, metric: 5n, hops } });
		expect(await announced.next()).toMatchObject({ prefix: room, kind: "offline" });
		await send({ status: "routeUpdate", id: 0n, advert: { ...ADVERT, metric: 5n } });
		expect(await announced.next()).toMatchObject({ prefix: room, kind: "start", route: { cost: 8n } });
	}

	// A route the peer lets go takes its claims offline, and is echoed back.
	await send({ status: "routeEnd", id: 0n });
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "offline" });
	expect(await decodeAnnounceReplyMaybe(replies, version)).toEqual({ kind: "echo", id: 0n });
	// Its Announce IDs went with it.
	await send({ status: "endedId", id: 0n });
	await expect(announced.next()).rejects.toThrow(ProtocolViolation);

	subscriber.close();
});

test("a lite-07 path served by two routes fails over when the cheaper one ends", async () => {
	const { subscriber, announced, send, replies, version } = await routedStream();
	const room = Path.from("room");
	const a = Instance.parse("a");
	const b = Instance.parse("b");

	await send({ status: "routeStart", node: NODE_A, advert: { ...ADVERT, metric: 5n } });
	await send({ status: "active", suffix: room, route: 0n, instance: a });
	await send({ status: "floor", id: 0n });
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "start", route: { instance: a, node: NODE_A } });

	// Another origin claims the path more cheaply: it takes over, as another instance.
	await send({ status: "routeStart", node: NODE_B, advert: ADVERT });
	await send({ status: "active", suffix: room, route: 1n, instance: b });
	await send({ status: "floor", id: 1n });
	expect(await announced.next()).toMatchObject({
		prefix: room,
		kind: "restart",
		route: { instance: b, node: NODE_B },
	});
	const consumed = subscriber.consume(room);

	// Its route ending falls back to the other, and the next consume names that origin.
	await send({ status: "routeEnd", id: 1n });
	expect(await announced.next()).toMatchObject({
		prefix: room,
		kind: "restart",
		route: { instance: a, node: NODE_A },
	});
	expect(await decodeAnnounceReplyMaybe(replies, version)).toEqual({ kind: "echo", id: 1n });
	expect(subscriber.consume(room).closed).not.toBe(consumed.closed);

	consumed.close();
	announced.close();
	subscriber.close();
});

test.each([
	[
		"a second route to one origin node",
		[
			{ status: "routeStart", node: NODE_A, advert: ADVERT },
			{ status: "routeStart", node: NODE_A, advert: ADVERT },
		],
	],
	[
		"a route claiming one path twice",
		[
			{ status: "routeStart", node: NODE_A, advert: ADVERT },
			{ status: "active", suffix: Path.from("room"), route: 0n, instance: Instance.parse("a") },
			{ status: "active", suffix: Path.from("room"), route: 0n, instance: Instance.parse("b") },
		],
	],
	[
		"a claim under an unknown route",
		[{ status: "active", suffix: Path.from("room"), route: 0n, instance: Instance.parse("a") }],
	],
	["a floor for an unknown route", [{ status: "floor", id: 0n }]],
] as [string, AnnounceBroadcast[]][])("lite-07 %s is a violation", async (_, messages) => {
	const version = Version.DRAFT_07;
	const { subscriber, send, settle, sessionEnded } = announceHarness(version);
	const announced = subscriber.routes();
	await settle();
	await send(async (w) => {
		await new AnnounceOk(PEER, 0).encode(w, version);
		for (const msg of messages) await encodeAnnounceBroadcast(w, msg, version);
	});
	await expect(announced.next()).rejects.toThrow(ProtocolViolation);
	expect((await sessionEnded())?.closeCode).toBe(SessionCode.ProtocolViolation);
	announced.close();
	subscriber.close();
});

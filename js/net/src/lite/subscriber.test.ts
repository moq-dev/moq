import { expect, jest, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { type Getter, Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import type { AuthSession } from "../auth_session.ts";
import type { Probe as ProbeStats } from "../connection/stats.ts";
import * as Epoch from "../epoch.ts";
import {
	error,
	fromTransport,
	ProtocolViolation,
	reason,
	SessionCode,
	StreamCode,
	StreamError,
	TimestampMismatch,
} from "../error.ts";
import { HopSchema, isAnonymous, MAX_HOPS, Route, UNKNOWN_HOP } from "../hop.ts";
import { hooks } from "../internal.ts";
import { GRACE } from "../ledger.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import type { Producer } from "../track.ts";
import { type AnnounceBroadcast, AnnounceInit, AnnounceOk, encodeAnnounceBroadcast } from "./announce.ts";
import { Datagram as DatagramMessage } from "./datagram.ts";
import { Group } from "./group.ts";
import { Probe } from "./probe.ts";
import { StreamId } from "./stream.ts";
import {
	encodeSubscribeResponse,
	SubscribeDrop,
	SubscribeEnd,
	SubscribeOk,
	type SubscribeResponse,
	SubscribeStart,
} from "./subscribe.ts";
import { SUBSCRIBE_SETUP_TIMEOUT_MS, Subscriber } from "./subscriber.ts";
import { Track, TrackInfo } from "./track.ts";
import { Version, versionName } from "./version.ts";

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
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (inbound = controller) }),
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
		let timer: ReturnType<typeof setTimeout> | undefined;
		const timeout = new Promise<never>((_, reject) => {
			timer = setTimeout(() => reject(new Error("stream was never aborted")), 250);
		});
		try {
			return reason(error(await Promise.race([aborted, timeout])));
		} finally {
			clearTimeout(timer);
		}
	};

	// Rejects promptly if the session outlives the violation, rather than hanging the test.
	const sessionEnded = async () => {
		let timer: ReturnType<typeof setTimeout> | undefined;
		const timeout = new Promise<never>((_, reject) => {
			timer = setTimeout(() => reject(new Error("session was never closed")), 250);
		});
		try {
			return await Promise.race([sessionClosed, timeout]);
		} finally {
			clearTimeout(timer);
		}
	};

	return {
		subscriber,
		send,
		// The peer FINs the announce stream.
		finish: () => inbound.close(),
		abortReason,
		sessionEnded,
		settle: () => new Promise((resolve) => setTimeout(resolve, 0)),
	};
}

const PUBLISHER_A = HopSchema.parse(7n);
const PUBLISHER_B = HopSchema.parse(8n);
const PUBLISHER_C = HopSchema.parse(9n);
const PEER = HopSchema.parse(2n);

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

	const announced = subscriber.announced(Path.Pattern.subtree(Path.from("room")));
	announced.close();
	// The slot frees up only after the consumer left.
	opening.resolve();
	await new Promise((resolve) => setTimeout(resolve, 0));
	expect(written).toBe(0);
	expect(closed).toBe(true);
});

test("a local empty hop chain is not anonymous", () => {
	expect(isAnonymous(Route.default)).toBe(false);
	expect(isAnonymous({ hops: [UNKNOWN_HOP], cost: Route.default.cost })).toBe(true);
});

test("a max-length chain plus withheld responder is dropped", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
	await settle();
	const room = Path.from("room");

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_07));
	const first = Epoch.mint();
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: room, epoch: first, hops: [PUBLISHER_A] },
			Version.DRAFT_07,
		),
	);
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "start", route: { epoch: first } });
	const held = subscriber.consume(room);

	// Another instance, under a new epoch or none: a restart, and the handle already out stays.
	for (const epoch of [Epoch.mint(), undefined]) {
		await send((w) =>
			encodeAnnounceBroadcast(w, { status: "restart", id: 0n, epoch, hops: [PUBLISHER_B] }, Version.DRAFT_07),
		);
		const restarted = await announced.next();
		expect(restarted).toMatchObject({ prefix: room, kind: "restart", route: { hops: [PUBLISHER_B, PEER] } });
		expect(restarted?.route.epoch).toBe(epoch);
		const fresh = subscriber.consume(room);
		expect(fresh.closed).not.toBe(held.closed);
		expect(held.closed.peek()).toBeUndefined();
		fresh.close();
	}

	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_07));
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
	const announced = subscriber.announced();
	await settle();
	const pool = Path.from("pool");
	const job = Path.from("pool/job");

	await send((w) => new AnnounceOk(PEER, 0).encode(w, version));
	await send((w) => encodeAnnounceBroadcast(w, { status: "active", suffix: pool, hops: [PUBLISHER_A] }, version));
	expect(await announced.next()).toMatchObject({ prefix: pool, kind: "start" });
	const held = subscriber.consume(job);

	if (version === Version.DRAFT_07) {
		await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_B] }, version));
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
		rtt: Milli(40),
	});
	expect(got.estimatedRecvRate).toBe(2_000_000);
	expect(got.rtt).toBe(Milli(40));
});

test("an announce skipped as a reflected loop still holds its path", async () => {
	// The subscriber's own origin, so a chain naming it reflects back through us.
	const SELF = 1n;
	const { subscriber, send, abortReason, sessionEnded, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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

test("an update keeps the epoch of an announce skipped as a reflected loop", async () => {
	const SELF = 1n;
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_07, SELF);
	const announced = subscriber.announced();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_07));

	// Skipped: the chain reflects back through us. The epoch still names the instance.
	const epoch = Epoch.mint();
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), epoch, hops: [HopSchema.parse(SELF)] },
			Version.DRAFT_07,
		),
	);
	await settle();

	// An update never carries the epoch, so the attached route takes the one the start named.
	await send((w) => encodeAnnounceBroadcast(w, { status: "update", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_07));
	const started = await announced.next();
	expect(started).toMatchObject({ prefix: Path.from("room"), kind: "start" });
	expect(started?.route.epoch).toBe(epoch);

	announced.close();
	subscriber.close();
});

test("retiring an id whose announce was skipped ends nothing", async () => {
	const SELF = 1n;
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
		const announced = subscriber.announced(Path.Pattern.subtree(half));
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
					written.push(new Uint8Array(chunk));
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
				close: () => onFinish(),
				abort: (reason) => void onAbort(reason),
			});
			const opened = new Promise((resolve) => (release = () => resolve({ readable, writable })));
			if (!park.includes(streams.length)) release();
			const stop = (reason: unknown) =>
				new Promise<void>((refused) => {
					stopped = { reason, refused };
				});
			streams.push({ inbound, reading, aborted, finished, writing, written, release, stop });
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

test.each([Version.DRAFT_06, Version.DRAFT_07])(
	"lite %s: oversized incoming datagrams are dropped",
	async (version) => {
		const installed = new Manual().install();
		const { quic, opened } = fakeSession();
		const incoming = new TransformStream<Uint8Array, Uint8Array>();
		Object.defineProperty(quic, "datagrams", { value: { readable: incoming.readable, maxDatagramSize: 1400 } });
		const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
		const track = subscriber.consume(Path.from("room")).track("video").subscribe();
		const writer = incoming.writable.getWriter();
		const receiving = subscriber.runDatagrams();
		try {
			await answerTrackInfo(await opened(0), version, 1000);
			const request = await opened(1);
			await request.reading;
			await respond(request, version, [{ start: new SubscribeStart(0) }]);
			const received = track.recvDatagram();
			await writer.write(new DatagramMessage(0n, 0, 0, new Uint8Array(1198)).encode(version));
			await writer.write(new DatagramMessage(0n, 1, 1, new Uint8Array(1197)).encode(version));
			expect((await received)?.sequence).toBe(1);
		} finally {
			await writer.close();
			await receiving;
			track.close();
			subscriber.close();
			installed.close();
		}
	},
);

function expectCut(err: unknown, cause: Error | undefined) {
	if (cause) {
		expect(err).toBe(cause);
	} else {
		expect(err).toBeInstanceOf(StreamError);
		expect((err as StreamError).code).toBe(StreamCode.SessionClosed);
	}
}

// Lite has no FETCH_OK, so a publisher that never answers holds each setup stage until the
// subscriber closes. The stream that stage opened is reset, even one opening after the close.
test.each([
	["the TRACK_INFO", "track", undefined],
	["the FETCH", "fetch", undefined],
	["the FETCH, on a session error", "fetch", new Error("session died")],
	["a stream slot for the FETCH", "open", undefined],
] as const)("closing the subscriber rejects a fetch waiting on %s", async (_, stage, cause) => {
	const { quic, streams, opened } = fakeSession(stage === "open" ? [1] : []);
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	let settled = false;
	const fetch = subscriber.fetchGroup(Path.from("room"), "video", 0).then(
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
	await stuck.aborted;
});

// A setup that outlived its deadline is over: its TRACK stream is reset, even one still waiting for a
// slot, and the subscription is neither registered again nor sent as a SUBSCRIBE.
test.each([
	["lite-05 subscribe waiting on the TRACK_INFO", Version.DRAFT_05, false],
	["lite-06 subscribe waiting on the TRACK_INFO", Version.DRAFT_06, false],
	["lite-07 subscribe waiting on the TRACK_INFO", Version.DRAFT_07, false],
	["lite-05 subscribe waiting on a stream slot for the TRACK", Version.DRAFT_05, true],
] as const)("a %s that times out leaves nothing behind", async (_, version, parked) => {
	jest.useFakeTimers();
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession(parked ? [0] : []);
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	try {
		const track = subscriber.consume(Path.from("room")).track("video").subscribe();
		await opened(0);
		if (!parked) await streams[0].reading;

		jest.advanceTimersByTime(SUBSCRIBE_SETUP_TIMEOUT_MS);
		await track.closed;

		if (parked) streams[0].release();
		await streams[0].aborted;
		expect(streams.length).toBe(1);

		// A GROUP for a forgotten id is ignored without touching its stream.
		const touched: PropertyKey[] = [];
		const reader = new Proxy({} as Reader, {
			get: (_, key) => {
				touched.push(key);
				return () => {};
			},
		});
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 0 }), reader);
		expect(touched).toEqual([]);
	} finally {
		subscriber.close();
		warn.mockRestore();
		jest.useRealTimers();
	}
});

// The SUBSCRIBE stream can open after the deadline too; it is reset without carrying a SUBSCRIBE.
test("a lite subscribe that times out waiting on a stream slot for the SUBSCRIBE sends nothing on it", async () => {
	jest.useFakeTimers();
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession([1]);
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	try {
		const track = subscriber.consume(Path.from("room")).track("video").subscribe();
		await opened(0);
		await streams[0].reading;
		// TRACK_INFO lands halfway, so the SUBSCRIBE open's own deadline is still ahead when the
		// setup deadline fires.
		jest.advanceTimersByTime(SUBSCRIBE_SETUP_TIMEOUT_MS / 2);
		await answerTrackInfo(streams[0]);
		await opened(1);

		jest.advanceTimersByTime(SUBSCRIBE_SETUP_TIMEOUT_MS / 2);
		await track.closed;

		streams[1].release();
		await streams[1].aborted;
		expect(streams[1].written).toEqual([]);
	} finally {
		subscriber.close();
		warn.mockRestore();
		jest.useRealTimers();
	}
});

// A SUBSCRIBE write blocked on flow control may never let the setup settle, so the deadline
// lets go of the held TRACK stream without waiting for it.
test("a lite subscribe whose SUBSCRIBE write stalls past the deadline closes its TRACK stream", async () => {
	jest.useFakeTimers();
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession([], [1]);
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	try {
		const track = subscriber.consume(Path.from("room")).track("video").subscribe();
		await opened(0);
		await streams[0].reading;
		await answerTrackInfo(streams[0]);
		await (await opened(1)).writing;

		jest.advanceTimersByTime(SUBSCRIBE_SETUP_TIMEOUT_MS);
		await track.closed;
		await streams[0].aborted;
	} finally {
		subscriber.close();
		warn.mockRestore();
		jest.useRealTimers();
	}
});

test("a fetch started after the subscriber closes rejects without opening a stream", async () => {
	const { quic, streams } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	subscriber.close();

	const err = await subscriber.fetchGroup(Path.from("room"), "video", 0).catch((err: unknown) => err);
	expectCut(err, undefined);
	expect(streams.length).toBe(0);
});

// The grant watch is armed before the first check, so a shrink while TRACK_INFO is still in
// flight refuses the subscription rather than opening a SUBSCRIBE the grant no longer covers.
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
	// Nothing past the TRACK stream reached the wire.
	expect(streams.length).toBe(1);

	track.close();
});

test("an already-aborted fetch rejects without opening a stream", async () => {
	const { quic, streams } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	const cause = new Error("gone");

	const err = await subscriber
		.fetchGroup(Path.from("room"), "video", 0, { signal: AbortSignal.abort(cause) })
		.catch((err: unknown) => err);
	expect(err).toBe(cause);
	expect(streams.length).toBe(0);
});

// Coalesced fetches share one FETCH stream. An abort releases only that caller's share; the
// stream is cancelled once the last sharer leaves, before its FETCH is sent if it can be.
test("one of two fetch sharers aborting leaves the other's fetch", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	const controller = new AbortController();
	const a = subscriber.fetchGroup(Path.from("room"), "video", 0, { signal: controller.signal });
	const b = subscriber.fetchGroup(Path.from("room"), "video", 0);

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
	const a = subscriber.fetchGroup(Path.from("room"), "video", 0, { signal: first.signal });
	const b = subscriber.fetchGroup(Path.from("room"), "video", 0, { signal: second.signal });

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
		// The abandoned TRACK exchange is reset promptly; no FETCH is sent.
		const err = fromTransport(await streams[0].aborted) as StreamError;
		expect(err.code).toBe(StreamCode.Cancel);
		expect(streams.length).toBe(1);
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
		const a = subscriber.fetchGroup(Path.from("room"), "video", 0, { signal: controller.signal });
		void a.catch(() => undefined);
		await opened(0);
		await answerTrackInfo(streams[0]);
		await opened(1);
		await streams[1].reading;

		controller.abort(new Error("gone"));
		for (let i = 0; i < depth; i++) await Promise.resolve();
		const b = subscriber.fetchGroup(Path.from("room"), "video", 0).catch((err: unknown) => err);
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
	const fetch = old.track("video").fetchGroup(0);
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
		.fetchGroup(0)
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

	const fetch = subscriber.fetchGroup(Path.from("room"), "video", 0);
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
		const pending = subscriber.fetchGroup(Path.from("room/cam"), "video", 0).then(
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
test.each([Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07])(
	"a retired epochless handle refuses new metadata on %s",
	async (version) => {
		const { quic, streams, opened } = fakeSession();
		const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
		const announced = subscriber.announced();
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

// A TRACKS request carries no epoch for an epochless route, so it would list the replacement.
test("a listing of an epochless handle retired while its token waits never reaches the wire", async () => {
	const { quic, streams, opened } = fakeSession();
	let release!: () => void;
	const released = new Promise<void>((resolve) => (release = resolve));
	const auth = {
		ready: (_path: Path.Valid, _direction: string, opts?: { signal?: AbortSignal }) =>
			new Promise<boolean>((resolve, reject) => {
				void released.then(() => resolve(true));
				opts?.signal?.addEventListener("abort", () => reject(opts.signal?.reason), { once: true });
			}),
	} as unknown as AuthSession;
	const version = Version.DRAFT_07;
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
	const announced = subscriber.announced();
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
	const tracks = held.tracks();
	const event = tracks.next().catch((err: unknown) => err);

	await send((writer) => encodeAnnounceBroadcast(writer, { status: "endedId", id: 0n }, version));
	await announced.next();
	await send((writer) =>
		encodeAnnounceBroadcast(writer, { status: "active", suffix: path, hops: [PUBLISHER_B] }, version),
	);
	await announced.next();
	release();

	const err = await event;
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.Unroutable);
	expect(streams.length).toBe(1);
	tracks.close();
	held.close();
	announced.close();
	subscriber.close();
});

// Announce PUBLISHER_A's epochless `pool` on stream 0, and return how to withdraw it and how to
// announce PUBLISHER_B's in its place.
async function epochlessPool(version: Version, streams: FakeStream[], announced: { next(): Promise<unknown> }) {
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
			encodeAnnounceBroadcast(writer, { status: "active", suffix: path, hops: [PUBLISHER_B] }, version),
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

// A SUBSCRIBE without an epoch resolves whichever instance won the path when it arrived, so one
// unanswered when another instance takes the path is refused, a withdrawal followed by a new
// announcement included. One answered first stays on its instance, and a withdrawal alone leaves
// it to its answer, so a track that finished as its broadcast ended still arrives.
test.each([
	["unanswered on lite-05 is refused", Version.DRAFT_05, "replaced"],
	["unanswered on lite-06 is refused", Version.DRAFT_06, "replaced"],
	["unanswered on lite-07 is refused", Version.DRAFT_07, "replaced"],
	["answered on lite-07 stays", Version.DRAFT_07, "answered"],
	["unanswered on lite-05 finishes", Version.DRAFT_05, "withdrawn"],
	["unanswered on lite-06 finishes", Version.DRAFT_06, "withdrawn"],
	["unanswered on lite-07 finishes", Version.DRAFT_07, "withdrawn"],
] as const)("an epochless SUBSCRIBE %s at a takeover or withdrawal", async (_, version, outcome) => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const announced = subscriber.announced();
	try {
		await opened(0);
		const pool = await epochlessPool(version, streams, announced);
		const held = subscriber.consume(Path.from("pool/job"));
		const track = held.track("video").subscribe();
		await answerTrackInfo(await opened(1), version);
		const subscribe = await opened(2);
		await subscribe.reading;

		if (outcome === "answered") {
			await respond(subscribe, version, [{ start: new SubscribeStart(0) }]);
			await drainReactions();
			await pool.replace();
			await drainReactions();
			expect(track.closed.peek()).toBeUndefined();
		} else if (outcome === "withdrawn") {
			await pool.withdraw();
			await drainReactions();
			expect(track.closed.peek()).toBeUndefined();
			await respond(subscribe, version, [{ start: new SubscribeStart(0) }, { end: new SubscribeEnd(0) }], true);
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

// A TRACKS request without an epoch lists whichever instance won the path when it arrived, so
// another instance taking the path refuses one still unanswered.
test("a listing of an epochless handle replaced before its answer is refused", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const announced = subscriber.announced();
	await opened(0);
	const pool = await epochlessPool(version, streams, announced);
	const held = subscriber.consume(Path.from("pool/job"));
	const tracks = held.tracks();
	expect(await tracks.next()).toEqual({ kind: "synced" });
	const event = tracks.next().catch((err: unknown) => err);
	const listing = await opened(1);
	await listing.reading;

	await pool.replace();
	const err = await event;
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.Unroutable);
	await listing.aborted;
	tracks.close();
	held.close();
	announced.close();
	subscriber.close();
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
	await new Promise((resolve) => setTimeout(resolve, 0));
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
		const first = subscriber.fetchGroup(path, "video", 0, {}, consumed).catch((err: unknown) => err);
		const second = subscriber.fetchGroup(path, "video", 0, {}, consumed).catch((err: unknown) => err);
		await opened(0);
		if (stage !== "track") {
			await answerTrackInfo(streams[0], Version.DRAFT_06);
			await opened(1);
		}
		const cause = new Error("broadcast instance retired");
		retired.abort(cause);
		expect(await first).toBe(cause);
		expect(await second).toBe(cause);
		if (stage === "open") streams[1].release();
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
	const fetch = subscriber.fetchGroup(path, "video", 0, {}, consumed);
	await opened(0);
	await answerTrackInfo(streams[0], Version.DRAFT_06);
	await opened(1);
	await streams[1].reading;
	streams[1].inbound.enqueue(new Uint8Array([0, 4, ...new TextEncoder().encode("old!")]));
	const first = await fetch;
	retired.abort(new Error("broadcast instance retired"));
	const cached = await subscriber.fetchGroup(path, "video", 0, {}, consumed);
	expect(streams.length).toBe(2);
	expect((await first.readFrame())?.payload).toEqual((await cached.readFrame())?.payload);
	first.close();
	cached.close();
	subscriber.close();
});

// Drive discovery and inspect the TRACK request before answering it. Waiting for the
// reader means the whole request has reached the wire, without a clock-based settle.
test("consumes resolve the serving prefix and pin its epoch", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const announced = subscriber.announced();
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
	const epoch = Epoch.mint();
	await encodeAnnounceBroadcast(peer, { status: "active", suffix: pool, epoch, hops: [PUBLISHER_A] }, version);
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
	expect((await Track.decode(reader, version)).epoch).toBe(epoch);

	// No epoch on the more specific source must shadow the covering source's epoch.
	await encodeAnnounceBroadcast(peer, { status: "active", suffix: job, hops: [PUBLISHER_B] }, version);
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
	expect((await Track.decode(freshReader, version)).epoch).toBeUndefined();

	await encodeAnnounceBroadcast(peer, { status: "endedId", id: 1n }, version);
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
		const announced = subscriber.announced();
		await settle();
		await send((w) => new AnnounceOk(PEER, 0).encode(w, version));
		const pool = Path.from("pool");
		const job = Path.from("pool/job");
		await send((w) => encodeAnnounceBroadcast(w, { status: "active", suffix: pool, hops: [PUBLISHER_A] }, version));
		await announced.next();
		const held = subscriber.consume(job);
		await send((w) => encodeAnnounceBroadcast(w, { status: "active", suffix: job, hops: [PUBLISHER_B] }, version));
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

test.each([Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07])(
	"pending metadata cancels on selection change and a live fallback can request again on %s",
	async (version) => {
		const { quic, streams, opened } = fakeSession();
		const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
		const announced = subscriber.announced();
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
		await encodeAnnounceBroadcast(peer, { status: "active", suffix: pool, hops: [PUBLISHER_A] }, version);
		await announced.next();
		const held = subscriber.consume(job);
		const pending = held
			.track("pending")
			.info()
			.catch((err: unknown) => err);
		await opened(1);
		await streams[1].reading;
		await encodeAnnounceBroadcast(peer, { status: "active", suffix: job, hops: [PUBLISHER_B] }, version);
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

test("a cancelled queued PROBE open sends no request and releases its slot", async () => {
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
	await streams[0].finished;
	expect(streams[0].written).toEqual([]);
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

test("a local unsubscribe owes no SUBSCRIBE_END", async () => {
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
			new Group({ subscribe: 0n, sequence: 0 }),
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
			...[entry.track.subscription, entry.track.demand().wanted, hooks.producerLargest(entry.track)].map(
				countChanges,
			),
		);
		return runResponses(stream, entry);
	});
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		await (await opened(1)).reading;
		await drainReactions();
		const before = counters.map((counter) => counter.count());
		expect(before).toHaveLength(3);
		expect(before.every((count) => count > 0)).toBe(true);
		for (let sequence = 0; sequence < 50; sequence++) {
			await subscriber.runGroup(new Group({ subscribe: 0n, sequence }), await groupStream(sequence, version));
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
			new Group({ subscribe: 0n, sequence: 0, frameStart }),
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
	await writer.u62(BigInt(timestamp) * 2n);
	await writer.u53(1);
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
// from media time, so it drops every timestamp there: a group that rewinds or lands out of order is
// accepted untimed.
test.each([Version.DRAFT_05, Version.DRAFT_06])("version %p drops received timestamps", async (version) => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	// Old enough to keep every group, however far behind the newest start.
	const track = broadcast.track("video").subscribe({ maxDelay: Milli(60_000) });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 4 }), await groupStream(1000, version));
		// Starts before the group below it.
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 5 }), await groupStream(500, version));
		// Lands below group 5 but starts after it.
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 3 }), await groupStream(2000, version));
		const got = [];
		for (let i = 0; i < 3; i++) {
			const group = await track.recvGroup();
			const frame = await group?.readFrame();
			got.push([group?.sequence, frame?.payload.length, frame?.timestamp]);
		}
		expect(got).toEqual([
			[3, 1, undefined],
			[4, 1, undefined],
			[5, 1, undefined],
		]);
		expect((await track.info()).timescale).toBeUndefined();
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
	const track = broadcast.track("video").subscribe({ maxDelay: Milli(60_000) });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version, 1000);
		await (await opened(1)).reading;
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 4 }), await groupStream(1000, version));
		// Starts before the group below it, so its stream is stopped with the refusal.
		const rewound = await groupStream(500, version);
		const stop = spyOn(rewound, "stop");
		await subscriber.runGroup(new Group({ subscribe: 0n, sequence: 5 }), rewound);
		expect(stop.mock.calls[0]?.[0]).toBeInstanceOf(TimestampMismatch);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

test("a publisher FIN without SUBSCRIBE_END still violates an open subscription", async () => {
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
		expect((track.closed.peek() as Error).message).toBe("subscribe stream ended without SUBSCRIBE_END");
		await drainReactions();
		expect(closes.map((info) => info?.closeCode)).toEqual([SessionCode.ProtocolViolation]);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
	}
});

// A lite-07 publisher FINs a bounded subscription without SUBSCRIBE_END once it covered the range,
// which ends the subscription cleanly rather than violating the protocol. Covering a range is
// not ending the track, so no end is declared.
test("a lite-07 publisher FIN ends a covered bounded subscription", async () => {
	const version = Version.DRAFT_07;
	const { quic, streams, opened, closes } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe({ groups: { start: { included: 0 }, end: { excluded: 2 } } });
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
		await encodeSubscribeResponse(writer, { start: new SubscribeStart(0) }, version);
		await encodeSubscribeResponse(
			writer,
			{ drop: new SubscribeDrop({ start: 0, end: 1, error: StreamCode.NotFound }) },
			version,
		);
		for (const chunk of chunks) stream.inbound.enqueue(chunk);
		stream.inbound.close();
		expect(await track.closed).toBeNull();
		await expect(track.finished()).rejects.toThrow("track closed before its end was known");
		await drainReactions();
		expect(closes).toEqual([]);
	} finally {
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

		await clock.advance(Nano.fromMilli(GRACE));
		expect(await track.finished()).toEqual({ group: 0, frame: 0 });
		expect(await track.closed).toBeNull();
		expect(closes).toEqual([]);
	} finally {
		track.close();
		broadcast.close();
		subscriber.close();
		installed.close();
	}
});

// The FIN may answer the bounded range before the publisher read an update that widened it,
// so the track fails rather than ending clean short of the demand.
test.each([
	["an open widening after SUBSCRIBE_START", { start: { included: 0 } }, true, false],
	["a bounded widening without SUBSCRIBE_START", { start: { included: 0 }, end: { excluded: 5 } }, false, false],
	["a widening the publisher stopped reading", { start: { included: 0 }, end: { excluded: 5 } }, false, true],
] as const)("a lite-07 FIN short of %s fails the track", async (_, groups, start, stopped) => {
	const clock = new Manual();
	const installed = clock.install();
	const version = Version.DRAFT_07;
	const { quic, streams, opened, closes } = fakeSession();
	const subscriber = new Subscriber(quic, version, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe({ groups: { start: { included: 0 }, end: { excluded: 1 } } });
	try {
		await opened(0);
		await answerTrackInfo(streams[0], version);
		const stream = await opened(1);
		await stream.reading;
		const refused = stopped
			? stream.stop(
					Object.assign(new Error("The stream was aborted by the remote server"), {
						source: "stream",
						streamErrorCode: StreamCode.Cancel,
					}),
				)
			: undefined;
		track.update({ groups });
		await refused;
		await drainReactions();

		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);
		if (start) {
			await encodeSubscribeResponse(writer, { start: new SubscribeStart(0) }, version);
			await encodeSubscribeResponse(
				writer,
				{ drop: new SubscribeDrop({ start: 0, end: 0, error: StreamCode.NotFound }) },
				version,
			);
		}
		for (const chunk of chunks) stream.inbound.enqueue(chunk);
		stream.inbound.close();
		await drainReactions();

		await clock.advance(Nano.fromMilli(GRACE));
		expect(await track.closed).toBeInstanceOf(Error);
		expect(closes).toEqual([]);
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
			await encodeSubscribeResponse(writer, { end: new SubscribeEnd(0) }, version);
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
// track. Lite-05 sends the start group plus one, past the largest u53 here.
test("an update whose bound the wire can't carry fails the track", async () => {
	const { quic, streams, opened } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	try {
		await opened(0);
		await answerTrackInfo(streams[0], Version.DRAFT_05);
		await (await opened(1)).reading;
		track.update({ groups: { start: { included: Number.MAX_SAFE_INTEGER } } });
		expect(await track.closed).toBeInstanceOf(RangeError);
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
		streams[1].inbound.enqueue(new Uint8Array([3, 0]));
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
		await drainReactions();
		expect(streams).toHaveLength(1);
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
	const announced = subscriber.announced();
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
	const announced = subscriber.announced();
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
		const announced = subscriber.announced();
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

test("a version without a boundary syncs once the request is out", async () => {
	const { subscriber, settle } = announceHarness(Version.DRAFT_03);
	const announced = subscriber.announced();
	await settle();
	expect(announced.synced.peek()).toBe(true);

	announced.close();
	subscriber.close();
});

test("announce init is the initial set", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_02);
	const announced = subscriber.announced();
	await settle();

	await send((w) => new AnnounceInit([Path.from("room")]).encode(w, Version.DRAFT_02));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });
	void announced.next();
	await settle();
	expect(announced.synced.peek()).toBe(true);

	announced.close();
	subscriber.close();
});

test.each(["answer", "credit"] as const)("FETCH shares the TRACK deadline while waiting for %s", async (stage) => {
	const clock = new Manual();
	const installed = clock.install();
	const { quic, streams, opened } = fakeSession(stage === "credit" ? [1] : []);
	const subscriber = new Subscriber(quic, Version.DRAFT_07, HopSchema.parse(1n));
	let failure: unknown;
	const fetch = subscriber.fetchGroup(Path.from("room"), "video", 0).catch((err: unknown) => {
		failure = err;
	});
	try {
		await opened(0);
		await streams[0].reading;
		await clock.advance(Nano.fromMilli(Milli(5_000)));
		await answerTrackInfo(streams[0], Version.DRAFT_07);
		await opened(1);
		if (stage === "answer") await streams[1].reading;
		await clock.advance(Nano.fromMilli(Milli(4_999)));
		expect(failure).toBeUndefined();
		await clock.advance(Nano.fromMilli(Milli(1)));
		await fetch;
		if (stage === "answer") expect((failure as StreamError).code).toBe(StreamCode.ControlTimeout);
		else {
			expect((failure as Error).name).toBe("NoStreamCredit");
			streams[1].release();
			await streams[1].aborted;
			expect(streams[1].written).toEqual([]);
		}
	} finally {
		subscriber.close();
		installed.close();
	}
});

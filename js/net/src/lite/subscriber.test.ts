import { expect, spyOn, test } from "bun:test";
import { Signal } from "@moq/signals";
import type { Probe as ProbeStats } from "../connection/stats.ts";
import * as Epoch from "../epoch.ts";
import { error, fromTransport, reason, StreamCode, StreamError } from "../error.ts";
import { HopSchema, isAnonymous, MAX_HOPS, Route, UNKNOWN_HOP } from "../hop.ts";
import * as Path from "../path.ts";
import { Writer } from "../stream.ts";
import * as Time from "../time.ts";
import { type AnnounceBroadcast, AnnounceInit, AnnounceOk, encodeAnnounceBroadcast } from "./announce.ts";
import { Probe } from "./probe.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackInfo } from "./track.ts";
import { Version } from "./version.ts";

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
		abortReason,
		sessionEnded,
		settle: () => new Promise((resolve) => setTimeout(resolve, 0)),
	};
}

const PUBLISHER_A = HopSchema.parse(7n);
const PUBLISHER_B = HopSchema.parse(8n);
const PUBLISHER_C = HopSchema.parse(9n);
const PEER = HopSchema.parse(2n);

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

test("a received hop list naming no publisher is stamped per connection", async () => {
	const first = async () => {
		const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
		const announced = subscriber.announced();
		await settle();

		await send((w) => new AnnounceOk(UNKNOWN_HOP, 0).encode(w, Version.DRAFT_06));
		await send((w) =>
			encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room"), hops: [] }, Version.DRAFT_06),
		);
		const update = await announced.next();
		expect(update).toMatchObject({ prefix: Path.from("room"), kind: "start" });
		const hops = update?.route.hops ?? [];
		expect(hops).toHaveLength(2);
		expect(hops[0]).not.toBe(UNKNOWN_HOP);
		// The 0 after the stamp keeps the unnamed publisher ranked as anonymous.
		expect(hops[1]).toBe(UNKNOWN_HOP);
		expect(update && isAnonymous(update.route)).toBe(true);

		announced.close();
		subscriber.close();
		return hops[0];
	};

	// A reconnect is a new connection, so the same unnamed publisher reads as a new one.
	expect(await first()).not.toBe(await first());
});

test("a received chain starting with hop 0 keeps it behind the stamp", async () => {
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
	const [stamp, ...rest] = update?.route.hops ?? [];
	expect(stamp).not.toBe(UNKNOWN_HOP);
	expect(rest).toEqual([UNKNOWN_HOP, PUBLISHER_A, PEER]);

	announced.close();
	subscriber.close();
});

test("a restart updates the route in place, even from another publisher", async () => {
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
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_06));

	// A reprice from the same publisher surfaces, which proves the identical restart above
	// emitted nothing. The same publisher keeps sharing one broadcast.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "restart", id: 0n, hops: [PUBLISHER_A], cost: { warm: 4n, cold: 4n } },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "update" });
	const same = subscriber.consume(room);
	expect(same.closed).toBe(held.closed);

	// A different publisher took the path: an in-place update, never a retraction.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_B] }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({
		prefix: room,
		kind: "update",
		route: { hops: [PUBLISHER_B, PEER] },
	});

	// The path still names the same broadcast, so the next consume shares it.
	expect(subscriber.consume(room).closed).toBe(held.closed);
	expect(held.closed.peek()).toBeUndefined();

	// A third publisher is another update, still the same broadcast.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_C] }, Version.DRAFT_06));
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

test("a restart that re-prices the same publisher emits the new route", async () => {
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
		route: { hops: [PUBLISHER_A, PEER], cost: { warm: 0n, cold: 0n } },
	});

	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "restart", id: 0n, hops: [PUBLISHER_A], cost: { warm: 4n, cold: 4n } },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("room"),
		kind: "update",
		route: { hops: [PUBLISHER_A, PEER], cost: { warm: 4n, cold: 4n } },
	});

	announced.close();
	subscriber.close();
});

test("a lite-05 duplicate announce follows the same restart rule", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_05);
	const announced = subscriber.announced();
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_05));
	const active = (hops: ReturnType<typeof HopSchema.parse>[]) => (w: Writer) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room"), hops }, Version.DRAFT_05);

	await send(active([PUBLISHER_A]));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	// On lite-05 a restart travels as a duplicate ANNOUNCE rather than its own message.
	await send(active([PUBLISHER_A]));
	await send(active([PUBLISHER_B]));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "update" });

	announced.close();
	subscriber.close();
});

// A responder that withholds its Hop ID sends the reserved 0, and an empty chain means it
// originated the path itself, so the advertisement names nobody and the connection stamps it.
// A restart on the same connection keeps that stamp, so it is a reprice that updates in place
// and keeps the shared broadcast, as does one that names a publisher.
test("a restart from an unidentified publisher updates in place", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.announced();
	await settle();
	const room = Path.from("room");

	await send((w) => new AnnounceOk(UNKNOWN_HOP, 0).encode(w, Version.DRAFT_06));
	await send((w) => encodeAnnounceBroadcast(w, { status: "active", suffix: room, hops: [] }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "start" });
	const held = subscriber.consume(room);

	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "restart", id: 0n, hops: [], cost: { warm: 4n, cold: 4n } },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toMatchObject({ prefix: room, kind: "update" });
	expect(subscriber.consume(room).closed).toBe(held.closed);

	// Naming a publisher changes the route, not the broadcast.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_06));
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

/** Bound on the microtask turns we will spend waiting for the decode loop. */
const MAX_DRAIN_TURNS = 1000;

/** Yield until `predicate` holds, rather than guessing a fixed number of turns. */
async function drainUntil(predicate: () => boolean): Promise<void> {
	for (let i = 0; i < MAX_DRAIN_TURNS; i++) {
		if (predicate()) return;
		await Promise.resolve();
	}
	throw new Error("probe messages never drained");
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
	await drainUntil(() => signal.peek().estimatedRecvRate === last.bitrate);
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
		rtt: Time.Milli(40),
	});
	expect(got.estimatedRecvRate).toBe(2_000_000);
	expect(got.rtt).toBe(Time.Milli(40));
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
	expect((await sessionEnded())?.closeCode).toBe(15);

	announced.close();
	subscriber.close();
});

test("a restart replaces an announce that was skipped as a reflected loop", async () => {
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

	// The id stays live, so the peer may restart it into a route that is usable here.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	// Retiring the id ends what the restart attached, and nothing else.
	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_06));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "end" });

	announced.close();
	subscriber.close();
});

test("a restart keeps the epoch of an announce skipped as a reflected loop", async () => {
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

	// A restart never carries the epoch, so the attached route takes the one the start named.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_07));
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
	expect((await sessionEnded())?.closeCode).toBe(15);

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
	expect(info?.closeCode).toBe(15);
	expect(new TextEncoder().encode(info?.reason ?? "").byteLength).toBeLessThanOrEqual(1024);
});

test("a draft-04 duplicate start is a violation, not a restart", async () => {
	// Pre-lite-05 has no replacement idiom, so a second ANNOUNCE_START for a live path is
	// the same violation lite-06 treats it as. Only lite-05, where the duplicate *is* the
	// restart, is exempt. The Rust loop routes every other version through `start_announce`,
	// which rejects it.
	const { subscriber, send, sessionEnded, settle } = announceHarness(Version.DRAFT_04);
	const announced = subscriber.announced();
	await settle();

	const active: AnnounceBroadcast = { status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] };
	await send((w) => encodeAnnounceBroadcast(w, active, Version.DRAFT_04));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	await send((w) => encodeAnnounceBroadcast(w, active, Version.DRAFT_04));

	// Session first: it is the bounded assertion, so a version that treats this as a
	// restart fails here in 250ms instead of hanging on a rejection that never comes.
	expect((await sessionEnded())?.closeCode).toBe(15);
	await expect(announced.next()).rejects.toThrow("duplicate announce");
});

test("a draft-05 duplicate start is still a restart", async () => {
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
	// Hands the stream to the subscriber, for an open the session was told to park.
	release: () => void;
}

// A session whose streams the test answers by hand and that never fails them on its own, so
// only Subscriber.close() can end a wait. Opens numbered in `park` wait for `release()`.
function fakeSession(park: number[] = []) {
	const streams: FakeStream[] = [];
	const quic = {
		createBidirectionalStream: () => {
			let inbound!: ReadableStreamDefaultController<Uint8Array>;
			let onRead!: () => void;
			let onAbort!: (reason: unknown) => void;
			let release!: () => void;
			const reading = new Promise<void>((resolve) => (onRead = resolve));
			const aborted = new Promise<unknown>((resolve) => (onAbort = resolve));
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
			const writable = new WritableStream<Uint8Array>({ abort: (reason) => void onAbort(reason) });
			const opened = new Promise((resolve) => (release = () => resolve({ readable, writable })));
			if (!park.includes(streams.length)) release();
			streams.push({ inbound, reading, aborted, release });
			return opened;
		},
	} as unknown as WebTransport;
	return { quic, streams };
}

async function answerTrackInfo(stream: FakeStream): Promise<void> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
		Version.DRAFT_05,
	);
	await new TrackInfo({}).encode(writer, Version.DRAFT_05);
	for (const chunk of chunks) stream.inbound.enqueue(chunk);
	stream.inbound.close();
}

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
	const { quic, streams } = fakeSession(stage === "open" ? [1] : []);
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

	await drainUntil(() => streams.length === 1);
	if (stage === "track") {
		await streams[0].reading;
	} else {
		await answerTrackInfo(streams[0]);
		await drainUntil(() => streams.length === 2);
		if (stage === "fetch") await streams[1].reading;
	}
	expect(settled).toBe(false);

	subscriber.close(cause);
	expectCut(await fetch, cause);

	const stuck = streams[stage === "track" ? 0 : 1];
	stuck.release();
	await stuck.aborted;
});

test("a fetch started after the subscriber closes rejects without opening a stream", async () => {
	const { quic, streams } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));
	subscriber.close();

	const err = await subscriber.fetchGroup(Path.from("room"), "video", 0).catch((err: unknown) => err);
	expectCut(err, undefined);
	expect(streams.length).toBe(0);
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
	const { quic, streams } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	const controller = new AbortController();
	const a = subscriber.fetchGroup(Path.from("room"), "video", 0, { signal: controller.signal });
	const b = subscriber.fetchGroup(Path.from("room"), "video", 0);

	await drainUntil(() => streams.length === 1);
	await answerTrackInfo(streams[0]);
	await drainUntil(() => streams.length === 2);
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
	const { quic, streams } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	const first = new AbortController();
	const second = new AbortController();
	const a = subscriber.fetchGroup(Path.from("room"), "video", 0, { signal: first.signal });
	const b = subscriber.fetchGroup(Path.from("room"), "video", 0, { signal: second.signal });

	await drainUntil(() => streams.length === 1);
	await streams[0].reading;
	if (stage === "fetch") {
		await answerTrackInfo(streams[0]);
		await drainUntil(() => streams.length === 2);
		await streams[1].reading;
	}

	first.abort(new Error("first"));
	second.abort(new Error("second"));
	expect(((await a.catch((err: unknown) => err)) as Error).message).toBe("first");
	expect(((await b.catch((err: unknown) => err)) as Error).message).toBe("second");

	if (stage === "track") {
		// The TRACK_INFO still completes, but no FETCH is sent for the abandoned group.
		await answerTrackInfo(streams[0]);
		for (let i = 0; i < 100; i++) await Promise.resolve();
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
		const { quic, streams } = fakeSession();
		const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

		const controller = new AbortController();
		const a = subscriber.fetchGroup(Path.from("room"), "video", 0, { signal: controller.signal });
		void a.catch(() => undefined);
		await drainUntil(() => streams.length === 1);
		await answerTrackInfo(streams[0]);
		await drainUntil(() => streams.length === 2);
		await streams[1].reading;

		controller.abort(new Error("gone"));
		for (let i = 0; i < depth; i++) await Promise.resolve();
		const b = subscriber.fetchGroup(Path.from("room"), "video", 0).catch((err: unknown) => err);
		for (let i = 0; i < 64; i++) await Promise.resolve();

		// An empty-group FIN accepts whichever FETCH the late caller is waiting on.
		if (streams.length > 2) {
			await answerTrackInfo(streams[2]);
			await drainUntil(() => streams.length === 4);
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

// A reader leaving partway through a fetched group cancels the FETCH: the truncated group
// must not end clean, as a FIN would make it read whole.
test("the last reader leaving mid-response cancels the fetch", async () => {
	const { quic, streams } = fakeSession();
	const subscriber = new Subscriber(quic, Version.DRAFT_05, HopSchema.parse(1n));

	const fetch = subscriber.fetchGroup(Path.from("room"), "video", 0);
	await drainUntil(() => streams.length === 1);
	await answerTrackInfo(streams[0]);
	await drainUntil(() => streams.length === 2);
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

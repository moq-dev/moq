import { expect, spyOn, test } from "bun:test";
import { Signal } from "@moq/signals";
import type { Probe as ProbeStats } from "../connection/stats.ts";
import { error, reason } from "../error.ts";
import { OriginSchema } from "../origin.ts";
import * as Path from "../path.ts";
import { Writer } from "../stream.ts";
import * as Time from "../time.ts";
import { AnnounceInit, AnnounceOk, encodeAnnounceBroadcast } from "./announce.ts";
import { Probe } from "./probe.ts";
import { Subscriber } from "./subscriber.ts";
import { Version } from "./version.ts";

test("closing the subscriber suppresses probe stream warnings", async () => {
	let readable!: ReadableStreamDefaultController<Uint8Array>;
	const quic = {
		createBidirectionalStream: async () => ({
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (readable = controller) }),
			writable: new WritableStream<Uint8Array>(),
		}),
	} as unknown as WebTransport;
	const subscriber = new Subscriber(quic, Version.DRAFT_03, OriginSchema.parse(1n), new Signal<ProbeStats>({}));
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
	let onSessionClose!: () => void;
	const sessionClosed = new Promise<void>((resolve) => (onSessionClose = resolve));
	const quic = {
		createBidirectionalStream: async () => ({
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (inbound = controller) }),
			writable: new WritableStream<Uint8Array>({ abort: (reason) => void onAbort(reason) }),
		}),
		close: () => void onSessionClose(),
	} as unknown as WebTransport;

	const subscriber = new Subscriber(quic, version, OriginSchema.parse(origin));

	const send = async (f: (w: Writer) => Promise<void>) => {
		const written: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
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
			await Promise.race([sessionClosed, timeout]);
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

const PUBLISHER_A = OriginSchema.parse(7n);
const PUBLISHER_B = OriginSchema.parse(8n);
const PUBLISHER_C = OriginSchema.parse(9n);
const PEER = OriginSchema.parse(2n);

test("a restart from the same publisher is a route change, not a republish", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06);
	const announced = subscriber.announced(Path.empty());
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [PUBLISHER_A] },
			Version.DRAFT_06,
		),
	);
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: true });

	// Same publisher over a new route. In-flight subscriptions resume across it, so the
	// subscriber must not surface anything that would make a consumer re-subscribe.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_06));

	// A different publisher took the path: nothing carries over, so this one does surface,
	// as an end before the start. Reaching it proves the reroute above emitted nothing.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_B] }, Version.DRAFT_06));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: false });
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: true });

	// A third publisher takes over. The replacement above has to leave its own publisher on
	// record, or this one reads as a first announcement and skips the end.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_C] }, Version.DRAFT_06));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: false });
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: true });

	// And the new owner's own reroute is still transparent.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_C] }, Version.DRAFT_06));
	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_06));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: false });

	announced.close();
	subscriber.close();
});

test("a lite-05 duplicate announce follows the same restart rule", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_05);
	const announced = subscriber.announced(Path.empty());
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_05));
	const active = (hops: ReturnType<typeof OriginSchema.parse>[]) => (w: Writer) =>
		encodeAnnounceBroadcast(w, { status: "active", suffix: Path.from("room"), hops }, Version.DRAFT_05);

	await send(active([PUBLISHER_A]));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: true });

	// On lite-05 a restart travels as a duplicate ANNOUNCE rather than its own message.
	await send(active([PUBLISHER_A]));
	await send(active([PUBLISHER_B]));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: false });
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: true });

	announced.close();
	subscriber.close();
});

// Encode PROBE messages the way a publisher would, so the subscriber's own loop
// decodes them.
async function probeBytes(probes: Probe[], version: Version): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
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
	const subscriber = new Subscriber(quic, version, OriginSchema.parse(1n), signal);
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
	const announced = subscriber.announced(Path.empty());
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));

	// Announce id 0: the chain loops back through us, so it is skipped locally. The peer
	// numbered it regardless and still holds the path.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [OriginSchema.parse(SELF)] },
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
	// repeat the violation on the next one.
	await sessionEnded();

	announced.close();
	subscriber.close();
});

test("a restart replaces an announce that was skipped as a reflected loop", async () => {
	const SELF = 1n;
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.announced(Path.empty());
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));

	// Skipped: the chain reflects back through us.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [OriginSchema.parse(SELF)] },
			Version.DRAFT_06,
		),
	);
	await settle();

	// The id stays live, so the peer may restart it into a route that is usable here.
	await send((w) => encodeAnnounceBroadcast(w, { status: "restart", id: 0n, hops: [PUBLISHER_A] }, Version.DRAFT_06));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: true });

	// Retiring the id ends what the restart attached, and nothing else.
	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_06));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: false });

	announced.close();
	subscriber.close();
});

test("retiring an id whose announce was skipped ends nothing", async () => {
	const SELF = 1n;
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.announced(Path.empty());
	await settle();

	await send((w) => new AnnounceOk(PEER, 0).encode(w, Version.DRAFT_06));

	// Id 0 for "room": skipped as a reflected loop.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [OriginSchema.parse(SELF)] },
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
	expect(await announced.next()).toEqual({ path: Path.from("lobby"), active: true });

	// Retire the skipped one, then the live one. The first must surface nothing, so the
	// only end a consumer sees is "lobby".
	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 0n }, Version.DRAFT_06));
	await send((w) => encodeAnnounceBroadcast(w, { status: "endedId", id: 1n }, Version.DRAFT_06));
	expect(await announced.next()).toEqual({ path: Path.from("lobby"), active: false });

	announced.close();
	subscriber.close();
});

test("a draft-02 initial announcement can still be retracted", async () => {
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_02);
	const announced = subscriber.announced(Path.empty());
	await settle();

	// ANNOUNCE_INIT carries the initial set. These are advertisements like any other, so
	// the peer may retract one later and the consumer has to hear about it.
	await send((w) => new AnnounceInit([Path.from("room")]).encode(w, Version.DRAFT_02));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: true });

	await send((w) => encodeAnnounceBroadcast(w, { status: "ended", suffix: Path.from("room") }, Version.DRAFT_02));
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: false });

	announced.close();
	subscriber.close();
});

test("a duplicate start is reported even when its own route reflects", async () => {
	const SELF = 1n;
	const { subscriber, send, settle } = announceHarness(Version.DRAFT_06, SELF);
	const announced = subscriber.announced(Path.empty());
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
	expect(await announced.next()).toEqual({ path: Path.from("room"), active: true });

	// A second start for that path, carrying a route that loops back through us. Skipping
	// it must not pre-empt the violation: the peer sent two starts with no end between
	// them regardless of whether the second one's route was usable.
	await send((w) =>
		encodeAnnounceBroadcast(
			w,
			{ status: "active", suffix: Path.from("room"), hops: [OriginSchema.parse(SELF)] },
			Version.DRAFT_06,
		),
	);

	await expect(announced.next()).rejects.toThrow("duplicate announce");

	announced.close();
	subscriber.close();
});

test("a draft-02 initial set naming a path twice is refused", async () => {
	const { subscriber, send, abortReason, sessionEnded, settle } = announceHarness(Version.DRAFT_02);
	const announced = subscriber.announced(Path.empty());
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
	// repeat the violation on the next one.
	await sessionEnded();

	announced.close();
	subscriber.close();
});

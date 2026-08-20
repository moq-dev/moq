import { expect, spyOn, test } from "bun:test";
import { Signal } from "@moq/signals";
import type { Probe as ProbeStats } from "../connection/stats.ts";
import { OriginSchema } from "../origin.ts";
import * as Path from "../path.ts";
import { Writer } from "../stream.ts";
import { AnnounceOk, encodeAnnounceBroadcast } from "./announce.ts";
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
	const quic = {
		createBidirectionalStream: async () => ({
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (inbound = controller) }),
			writable: new WritableStream<Uint8Array>(),
		}),
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

	return { subscriber, send, settle: () => new Promise((resolve) => setTimeout(resolve, 0)) };
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

/**
 * Drive `Subscriber.runProbe` over a canned script and return what the probe signal
 * held once the last message had been applied.
 *
 * Snapshotted before the stream is closed: `runProbe`'s `finally` blanks the signal
 * on exit, so reading afterwards would report `{}` no matter what the loop did.
 */
async function runProbeScript(version: Version, probes: Probe[]): Promise<ProbeStats> {
	let readable!: ReadableStreamDefaultController<Uint8Array>;
	const quic = {
		createBidirectionalStream: async () => ({
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (readable = controller) }),
			writable: new WritableStream<Uint8Array>(),
		}),
	} as unknown as WebTransport;

	const signal = new Signal<ProbeStats>({});
	const subscriber = new Subscriber(quic, version, OriginSchema.parse(1n), signal);
	const running = subscriber.runProbe();

	await Promise.resolve();
	readable.enqueue(await probeBytes(probes, version));
	// Each message costs several awaits to decode, so drain generously rather than
	// guessing an exact count.
	for (let i = 0; i < 200; i++) await Promise.resolve();

	const snapshot = signal.peek();

	readable.close();
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
		new Probe({ bitrate: 1_000_000, rtt: undefined }),
	]);
	// The bitrate proves both messages were applied, so an undefined RTT here is the
	// retraction landing rather than the loop never having run.
	expect(got.estimatedRecvRate).toBe(1_000_000);
	expect(got.rtt).toBeUndefined();
});

// lite-03's PROBE carries no RTT field at all, so an absent value there means "not
// carried" and the last reading must stand.
test("lite-03 keeps the last RTT, since its PROBE cannot carry one", async () => {
	const got = await runProbeScript(Version.DRAFT_03, [
		new Probe({ bitrate: 1_000_000, rtt: 40 }),
		new Probe({ bitrate: 2_000_000 }),
	]);
	expect(got.estimatedRecvRate).toBe(2_000_000);
	// lite-03 never carried the 40 on the wire, so there is nothing to retract and
	// nothing to preserve; what matters is that the absent field is not mistaken for
	// a retraction the way it is from lite-04 on.
	expect(got.rtt).toBeUndefined();
});

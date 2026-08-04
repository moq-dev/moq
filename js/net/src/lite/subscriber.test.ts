import { expect, spyOn, test } from "bun:test";
import { Signal } from "@moq/signals";
import type { Probe } from "../connection/stats.ts";
import * as Path from "../path.ts";
import { Writer } from "../stream.ts";
import { AnnounceOk, encodeAnnounceBroadcast } from "./announce.ts";
import { OriginSchema } from "./origin.ts";
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
	const subscriber = new Subscriber(quic, Version.DRAFT_03, OriginSchema.parse(1n), new Signal<Probe>({}));
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

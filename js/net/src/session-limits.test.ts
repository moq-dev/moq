import { expect, spyOn, test } from "bun:test";
import { SessionCode, SessionError } from "./error.ts";
import { HopSchema, randomHop } from "./hop.ts";
import { NativeSession } from "./ietf/adapter.ts";
import { Publisher as IetfPublisher } from "./ietf/publisher.ts";
import { RequestOk } from "./ietf/request.ts";
import { Subscribe as IetfSubscribe } from "./ietf/subscribe.ts";
import { SubscribeNamespaceEntry } from "./ietf/subscribe_namespace.ts";
import { Subscriber as IetfSubscriber } from "./ietf/subscriber.ts";
import { Version as IetfVersion } from "./ietf/version.ts";
import { AnnounceOk, encodeAnnounceBroadcast } from "./lite/announce.ts";
import { Publisher as LitePublisher } from "./lite/publisher.ts";
import { Subscribe as LiteSubscribe } from "./lite/subscribe.ts";
import { Subscriber as LiteSubscriber } from "./lite/subscriber.ts";
import { Version as LiteVersion } from "./lite/version.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Stream, Writer } from "./stream.ts";
import type { Limits } from "./util/limits.ts";
import { wireOf } from "./wire.ts";

// Small caps keep each case fast; util/limits.test.ts pins the real defaults.
const limits: Limits = { announces: 8, subscriptions: 4 };

// A publisher serving one announced broadcast whose tracks are never accepted, so every
// subscription stays live, waiting on the track, without timers or media.
function pendingPublisher(protocol: "lite" | "ietf") {
	const pair = createMockTransportPair();
	const closing = spyOn(pair.server, "close");
	const quiet = [
		spyOn(console, "warn").mockImplementation(() => {}),
		spyOn(console, "debug").mockImplementation(() => {}),
	];
	const origin = new OriginProducer();
	const path = Path.from("test");
	const broadcast = origin.createBroadcast(path);
	broadcast.announce();
	// Serve tracks on demand, then never answer the request.
	void wireOf(broadcast).requested();
	const version = protocol === "lite" ? LiteVersion.DRAFT_06 : IetfVersion.DRAFT_22;
	const publisher =
		protocol === "lite"
			? new LitePublisher(pair.server, LiteVersion.DRAFT_06, randomHop(), origin.consume(), undefined, limits)
			: new IetfPublisher({
					quic: pair.server,
					session: new NativeSession(pair.server, IetfVersion.DRAFT_22, false),
					publish: origin.consume(),
					requiresSolicitation: false,
					limits,
				});
	const pending: Promise<void>[] = [];

	// Subscribe once, returning a cancel that resets the requester's half.
	const subscribe = (id: number): (() => void) => {
		let requester!: ReadableStreamDefaultController<Uint8Array>;
		const stream = new Stream({
			version,
			readable: new ReadableStream<Uint8Array>({
				start(controller) {
					requester = controller;
				},
			}),
			writable: new WritableStream<Uint8Array>(),
		});
		const request =
			publisher instanceof LitePublisher
				? publisher.runSubscribe(
						new LiteSubscribe({ id: BigInt(id), broadcast: path, track: "video", priority: 0 }),
						stream,
					)
				: publisher.runSubscribe(
						new IetfSubscribe({
							requestId: BigInt(id * 2),
							trackNamespace: path,
							trackName: "video",
							subscriberPriority: 0,
						}),
						stream,
					);
		pending.push(request.catch(() => {}));
		return () => requester.error(new Error("cancelled"));
	};

	const close = async () => {
		broadcast.close();
		origin.close();
		await Promise.all(pending);
		publisher.close();
		pair.client.close();
		pair.server.close();
		closing.mockRestore();
		for (const spy of quiet) spy.mockRestore();
	};

	return { closing, pending, subscribe, close };
}

test.each(["lite", "ietf"] as const)("%s refuses the first subscription beyond the session cap", async (protocol) => {
	const session = pendingPublisher(protocol);
	try {
		for (let id = 0; id < limits.subscriptions; id++) session.subscribe(id);
		expect(session.closing).not.toHaveBeenCalled();
		session.subscribe(limits.subscriptions);
		expect(session.closing).toHaveBeenCalledTimes(1);
		expect(session.closing.mock.calls[0]?.[0]?.closeCode).toBe(SessionCode.TooManyRequests);
	} finally {
		await session.close();
	}
});

test.each(["lite", "ietf"] as const)(
	"%s releases subscriptions cancelled before the track is answered",
	async (protocol) => {
		const session = pendingPublisher(protocol);
		try {
			const cancels = [];
			for (let id = 0; id < limits.subscriptions; id++) cancels.push(session.subscribe(id));
			for (const cancel of cancels) cancel();
			// Every cancelled request ends without the track ever being answered.
			await Promise.all(session.pending);

			session.subscribe(limits.subscriptions);
			expect(session.closing).not.toHaveBeenCalled();
		} finally {
			await session.close();
		}
	},
);

test.each(["lite", "ietf"] as const)("%s closes on the first advertisement beyond its cap", async (protocol) => {
	const pair = createMockTransportPair();
	const closing = spyOn(pair.server, "close");
	const warnings = spyOn(console, "warn").mockImplementation(() => {});
	const debug = spyOn(console, "debug").mockImplementation(() => {});
	const version = protocol === "lite" ? LiteVersion.DRAFT_06 : IetfVersion.DRAFT_22;
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(chunk.slice()) }),
		version,
	);
	if (protocol === "lite") await new AnnounceOk(HopSchema.parse(2n), 0).encode(writer, LiteVersion.DRAFT_06);
	else {
		await writer.u53(RequestOk.id);
		await new RequestOk({}).encode(writer, IetfVersion.DRAFT_22);
	}
	for (let index = 0; index <= limits.announces; index++) {
		// Hidden entries still occupy the peer's budget even though none is surfaced locally.
		const path = Path.from(`.hidden/${index}`);
		if (protocol === "lite")
			await encodeAnnounceBroadcast(writer, { status: "active", suffix: path, hops: [] }, LiteVersion.DRAFT_06);
		else {
			await writer.u53(SubscribeNamespaceEntry.id);
			await new SubscribeNamespaceEntry({ suffix: path }).encode(writer, IetfVersion.DRAFT_22);
		}
	}
	writer.close();
	await writer.closed;
	const bytes = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0));
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.length;
	}
	const readable = new ReadableStream<Uint8Array>({
		start(controller) {
			controller.enqueue(bytes);
			controller.close();
		},
	});
	const writable = new WritableStream<Uint8Array>();
	const subscriber =
		protocol === "lite"
			? new LiteSubscriber(
					{
						...pair.server,
						createBidirectionalStream: async () => ({ readable, writable }),
						close: pair.server.close.bind(pair.server),
					} as unknown as WebTransport,
					LiteVersion.DRAFT_06,
					HopSchema.parse(1n),
					undefined,
					undefined,
					undefined,
					undefined,
					limits,
				)
			: new IetfSubscriber({
					quic: pair.server,
					session: {
						version: IetfVersion.DRAFT_22,
						nextRequestId: async () => 0n,
						spendRequest: () => {},
						openBi: async () => new Stream({ version, readable, writable }),
						acceptBi: async () => undefined,
						close: () => pair.server.close(),
					},
					limits,
				});
	try {
		const announced = subscriber.announced();
		const ended = await announced.closed;
		expect(ended).toBeInstanceOf(SessionError);
		expect((ended as SessionError).code).toBe(SessionCode.TooManyRequests);
		expect(closing.mock.calls[0]?.[0]?.closeCode).toBe(SessionCode.TooManyRequests);
	} finally {
		subscriber.close();
		pair.client.close();
		pair.server.close();
		closing.mockRestore();
		warnings.mockRestore();
		debug.mockRestore();
	}
});

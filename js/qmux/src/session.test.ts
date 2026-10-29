import { afterEach, beforeEach, describe, expect, spyOn, test } from "bun:test";
import { type Installed, Manual, Milli, Nano } from "@moq/clock";
import { SessionError, StreamError } from "./error.ts";
import * as Frame from "./frame.ts";
import { DEFAULT_MAX_RECORD_SIZE, type TransportParams } from "./frame.ts";
import * as QMux from "./index.ts";
import Session, { type Config } from "./session.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";
import type { WebSocketLike, WebSocketStreamLike } from "./web-socket-stream.ts";

// A scripted peer standing in for the `WebSocketStream` transport. The test
// plays the remote end: it injects frames into the Session's readable and
// captures frames the Session writes. Installed as `globalThis.WebSocketStream`
// so `openWebSocketStream` (which Session uses) picks it up instead of the real
// ponyfill (it's a distinct class, so the `Native !== WebSocketStream` guard
// holds).
class FakePeer {
	static last: FakePeer | undefined;

	readonly url: string;
	readonly opened: Promise<{
		readable: ReadableStream<Uint8Array | ArrayBuffer | string>;
		writable: WritableStream<Uint8Array>;
		protocol: string;
		extensions: string;
	}>;
	readonly closed: Promise<{ closeCode?: number; reason?: string }>;
	#closedResolve: (info: { closeCode?: number; reason?: string }) => void;
	#closedReject: (err: Error) => void;
	#recv!: ReadableStreamDefaultController<Uint8Array | ArrayBuffer | string>;
	#writeBlocked: Promise<void> | undefined;
	#writeError: Error | undefined;
	#unblockWrites: (() => void) | undefined;
	#pongResolve?: () => void;

	/** Raw chunks the Session has written. */
	sent: Uint8Array[] = [];
	closeInfo: { closeCode?: number; reason?: string } | undefined;
	/** How many chunks the Session had written when it closed the socket. */
	sentAtClose: number | undefined;

	/** `options` mirrors the native `WebSocketStream` constructor, which
	 *  `openWebSocketStream` calls with `{protocols, signal}` — hence `protocol`
	 *  (the *negotiated* value, which only `Session.accept` tests set) rather than
	 *  a positional argument that call would fill in. */
	constructor(url: string, options?: { protocol?: string }) {
		const protocol = options?.protocol ?? "qmux-01";
		this.url = url;
		FakePeer.last = this;
		const readable = new ReadableStream<Uint8Array | ArrayBuffer | string>({
			start: (c) => {
				this.#recv = c;
			},
		});
		const writable = new WritableStream<Uint8Array>({
			write: async (chunk) => {
				await this.#writeBlocked;
				if (this.#writeError) throw this.#writeError;
				this.sent.push(chunk);
				if (this.#pongResolve && Frame.decodeRecord(chunk).some((frame) => frame.type === "ping_response")) {
					this.#pongResolve();
					this.#pongResolve = undefined;
				}
			},
		});
		this.opened = Promise.resolve({ readable, writable, protocol, extensions: "" });
		const closed = Promise.withResolvers<{ closeCode?: number; reason?: string }>();
		this.closed = closed.promise;
		this.#closedResolve = closed.resolve;
		this.#closedReject = closed.reject;
		// A real WebSocketStream's `closed` can reject; nothing may await it here
		// until the test does, so keep that from surfacing as an unhandled rejection.
		this.closed.catch(() => {});
	}

	close(info: { closeCode?: number; reason?: string } = {}) {
		this.closeInfo = info;
		this.sentAtClose ??= this.sent.length;
		this.#closedResolve(info);
	}
	/** The socket errored out, rather than closing — `WebSocketStream.closed` rejects. */
	error(err = new Error("socket error")) {
		this.#closedReject(err);
	}
	setHighWaterMark() {}
	/** Reject every later write, without the socket reporting that it closed. */
	failWrites(err = new Error("write failed")) {
		this.#writeError = err;
	}
	blockWrites() {
		const blocked = Promise.withResolvers<void>();
		this.#writeBlocked = blocked.promise;
		this.#unblockWrites = blocked.resolve;
	}
	unblockWrites() {
		this.#unblockWrites?.();
		this.#writeBlocked = undefined;
		this.#unblockWrites = undefined;
	}

	// --- test controls ---
	/** Wait until the session has handled all preceding inbound frames. */
	barrier(): Promise<void> {
		const pong = Promise.withResolvers<void>();
		this.#pongResolve = pong.resolve;
		this.send({ type: "ping_request", sequence: 0n });
		return pong.promise;
	}

	/** Inject a frame as if sent by the peer. */
	send(frame: Frame.Any) {
		this.#recv.enqueue(Frame.encode(frame, "qmux-01"));
	}
	/** Inject a raw text frame (invalid for QMux). */
	sendText(text: string) {
		this.#recv.enqueue(text);
	}
	/** Inject a raw binary record — bytes the encoder wouldn't normally emit
	 * (e.g. the no-length 0x30 datagram form). */
	sendRaw(bytes: Uint8Array) {
		this.#recv.enqueue(bytes);
	}
	/** Inject a binary record with the shape Chromium's native WebSocketStream
	 * uses for incoming binary messages. */
	sendArrayBuffer(frame: Frame.Any) {
		this.#recv.enqueue(Uint8Array.from(Frame.encode(frame, "qmux-01")).buffer);
	}
	/** All frames the Session has written so far, decoded. */
	received(): Frame.Any[] {
		return this.sent.flatMap((b) => Frame.decodeRecord(b));
	}
	has(type: Frame.Any["type"]): boolean {
		return this.received().some((f) => f.type === type);
	}
	count(type: Frame.Any["type"]): number {
		return this.received().filter((f) => f.type === type).length;
	}
}

/** A plain `WebSocket` as a server host hands it over: already open, with the
 *  subprotocol picked during the upgrade. `Session.accept` has to wrap this
 *  itself — there's no `WebSocketStream` in sight. */
class FakeServerSocket implements WebSocketLike {
	binaryType = "blob";
	bufferedAmount = 0;
	readyState = 1; // OPEN
	extensions = "";
	sent: Array<string | ArrayBufferLike | ArrayBufferView> = [];

	onopen: ((ev: unknown) => void) | null = null;
	onmessage: ((ev: { data: unknown }) => void) | null = null;
	onerror: ((ev: unknown) => void) | null = null;
	onclose: ((ev: { code?: number; reason?: string }) => void) | null = null;

	readonly url: string;
	readonly protocol: string;

	constructor(url: string, protocol: string) {
		this.url = url;
		this.protocol = protocol;
	}

	send(data: string | ArrayBufferLike | ArrayBufferView): void {
		this.sent.push(data);
	}
	close(): void {
		this.readyState = 3; // CLOSED
	}
}

/** Poll an observable condition with a bounded timeout (no fixed-yield sleeps). */
async function waitFor(check: () => boolean, timeoutMs = 1000): Promise<void> {
	const start = Date.now();
	while (!check()) {
		if (Date.now() - start > timeoutMs) throw new Error("timed out waiting for condition");
		await new Promise((resolve) => setTimeout(resolve, 0));
	}
}

/** Yield a few macrotask turns so queued reads and any pending promise rejections
 * settle (an unhandled rejection surfaces at the end of the current turn). */
async function settle(turns = 5): Promise<void> {
	for (let i = 0; i < turns; i++) {
		await new Promise((resolve) => setTimeout(resolve, 0));
	}
}

/** Drain queued microtasks so a just-issued claim() reaches its parked state.
 *  Parking is pure-microtask work (no I/O), so a bounded drain is deterministic. */
async function flushMicrotasks(turns = 10): Promise<void> {
	for (let i = 0; i < turns; i++) await Promise.resolve();
}

/** Settle a promise to its value or its rejection error, without leaking an
 *  unhandled rejection while the test arranges the teardown that resolves it. */
function settleTo<T>(p: Promise<T>): Promise<T | Error> {
	return p.then(
		(v) => v,
		(e) => e,
	);
}

/** The session-close frame the Session put on the wire, if any. A locally-detected
 *  violation still owes the peer an explanation, even though it settles `closed`
 *  as a failure on our side. The frame *type* distinguishes the graceful
 *  APPLICATION_CLOSE a `close()` sends from the CONNECTION_CLOSE a violation sends. */
function sentClose(peer: FakePeer): Frame.ApplicationClose | Frame.ConnectionClose | undefined {
	return peer.received().find((f) => f.type === "application_close" || f.type === "connection_close") as
		| Frame.ApplicationClose
		| Frame.ConnectionClose
		| undefined;
}

function sentCloseCode(peer: FakePeer): number | undefined {
	const frame = sentClose(peer);
	return frame ? Number(frame.code.value) : undefined;
}

/** Assert `closed` rejected the way the WebTransport contract requires of an
 *  abnormal end, and hand the error back for further checks. A fulfilled `closed`
 *  is the bug this guards: it makes a dropped session look like a clean shutdown. */
async function expectSessionFailure(session: Session): Promise<SessionError> {
	const settled = await settleTo(session.closed);
	expect(settled).toBeInstanceOf(SessionError);
	const err = settled as SessionError;
	// Shaped like the native WebTransportError so consumers can branch on it.
	expect(err.name).toBe("WebTransportError");
	expect(err.source).toBe("session");
	expect(err.streamErrorCode).toBeNull();
	return err;
}

function peerParams(overrides: Partial<TransportParams> = {}): TransportParams {
	return {
		maxIdleTimeout: 0n,
		initialMaxData: 1_000_000n,
		initialMaxStreamDataBidiLocal: 100_000n,
		initialMaxStreamDataBidiRemote: 100_000n,
		initialMaxStreamDataUni: 100_000n,
		initialMaxStreamsBidi: 100n,
		initialMaxStreamsUni: 100n,
		maxDatagramFrameSize: DEFAULT_MAX_RECORD_SIZE,
		maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
		resetStreamAt: false,
		...overrides,
	};
}

function connect(config?: Config): { session: Session; peer: FakePeer } {
	(globalThis as { WebSocketStream?: unknown }).WebSocketStream = FakePeer;
	// Idle timer off so a stray interval can't interfere with the test.
	// Bare version ALPNs are offered by default (requireProtocol defaults to false).
	const session = new Session("https://example/test", {
		config: { idleTimeout: null, ...config },
	});
	const peer = FakePeer.last as FakePeer;
	return { session, peer };
}

const ORIGINAL_WSS = (globalThis as { WebSocketStream?: unknown }).WebSocketStream;

describe("Session integration (scripted peer)", () => {
	afterEach(() => {
		if (ORIGINAL_WSS === undefined) {
			delete (globalThis as { WebSocketStream?: unknown }).WebSocketStream;
		} else {
			(globalThis as { WebSocketStream?: unknown }).WebSocketStream = ORIGINAL_WSS;
		}
	});

	test("handshake: sends TRANSPORT_PARAMETERS and resolves ready", async () => {
		const { session, peer } = connect();
		await session.ready;
		await waitFor(() => peer.has("transport_parameters"));
		session.close();
	});

	test("a uni stream write produces a STREAM frame on the wire", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// createUnidirectionalStream blocks until the peer's stream-count credit arrives.
		const writable = await session.createUnidirectionalStream();
		await writable.getWriter().write(new Uint8Array([1, 2, 3]));

		await waitFor(() => peer.has("stream"));
		const stream = peer.received().find((f) => f.type === "stream") as Frame.Data;
		expect(stream.data).toEqual(new Uint8Array([1, 2, 3]));
		session.close();
	});

	test("aborting after a write sends the transmitted byte count as final_size", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const writable = await session.createUnidirectionalStream();
		const writer = writable.getWriter();
		await writer.write(new Uint8Array([1, 2, 3]));
		await writer.abort(new Error("cancel"));

		await waitFor(() => peer.has("reset_stream"));
		const reset = peer.received().find((f) => f.type === "reset_stream") as Frame.ResetStream;
		expect(reset.finalSize).toBe(3n);
		session.close();
	});

	test("aborting with a WebTransportError sends its streamErrorCode", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const writable = await session.createUnidirectionalStream();
		await writable.abort(new StreamError(26, "RESET_STREAM"));

		await waitFor(() => peer.has("reset_stream"));
		const reset = peer.received().find((f) => f.type === "reset_stream") as Frame.ResetStream;
		expect(reset.code.value).toBe(26n);
		session.close();
	});

	test("aborting with a plain error sends code 0", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const writable = await session.createUnidirectionalStream();
		await writable.abort(new Error("cancel"));

		await waitFor(() => peer.has("reset_stream"));
		const reset = peer.received().find((f) => f.type === "reset_stream") as Frame.ResetStream;
		expect(reset.code.value).toBe(0n);
		session.close();
	});

	// Abort is local: a write parked on the peer's credit must not hold the
	// RESET_STREAM back until the peer grants credit it may never grant.
	for (const [kind, params] of [
		["stream", { initialMaxStreamDataUni: 0n }],
		["connection", { initialMaxData: 0n }],
	] as const) {
		test(`aborting a write parked on ${kind} credit sends RESET_STREAM at once`, async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams(params) });

			const writer = (await session.createUnidirectionalStream()).getWriter();
			const write = settleTo(writer.write(new Uint8Array(10)));
			await settle();

			const reason = new StreamError(7, "RESET_STREAM");
			await writer.abort(reason);
			expect(await write).toBe(reason);

			await waitFor(() => peer.has("reset_stream"));
			const reset = peer.received().find((f) => f.type === "reset_stream") as Frame.ResetStream;
			expect(reset.code.value).toBe(7n);
			expect(reset.finalSize).toBe(0n);
			expect(peer.has("stream")).toBe(false);
			session.close();
		});
	}

	test("aborting a write queued behind a wedged socket doesn't wait for it to drain", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });
		await waitFor(() => peer.has("transport_parameters"));
		const writer = (await session.createUnidirectionalStream()).getWriter();

		// The ping response sticks in the socket; our write queues behind it.
		peer.blockWrites();
		peer.send({ type: "ping_request", sequence: 1n });
		await settle();
		const write = settleTo(writer.write(new Uint8Array(10)));
		await settle();

		const reason = new StreamError(7, "RESET_STREAM");
		await writer.abort(reason);
		expect(await write).toBe(reason);

		peer.unblockWrites();
		await waitFor(() => peer.has("reset_stream"));
		expect(peer.has("stream")).toBe(false);
		session.close();
	});

	test("aborting a close queued behind a wedged socket resets the stream", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });
		await waitFor(() => peer.has("transport_parameters"));
		const writer = (await session.createUnidirectionalStream()).getWriter();
		await writer.write(new Uint8Array(3));
		await waitFor(() => peer.has("stream"));

		// The ping response sticks in the socket; the FIN queues behind it.
		peer.blockWrites();
		peer.send({ type: "ping_request", sequence: 1n });
		await settle();
		const closed = settleTo(writer.close());
		await settle();

		await writer.abort(new StreamError(7, "RESET_STREAM"));
		expect(await closed).toBeUndefined();

		peer.unblockWrites();
		await waitFor(() => peer.has("reset_stream"));
		const reset = peer.received().find((f) => f.type === "reset_stream") as Frame.ResetStream;
		expect(reset.code.value).toBe(7n);
		expect(reset.finalSize).toBe(3n);
		expect(peer.received().some((f) => f.type === "stream" && f.fin)).toBe(false);
		session.close();
	});

	test("aborting mid-write sends no more of that write", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });
		await waitFor(() => peer.has("transport_parameters"));
		const writer = (await session.createUnidirectionalStream()).getWriter();

		// Three frames' worth, with the first stuck in the socket.
		peer.blockWrites();
		const write = settleTo(writer.write(new Uint8Array(3 * Number(DEFAULT_MAX_RECORD_SIZE))));
		await settle();
		const aborted = writer.abort(new Error("cancel"));
		await settle();
		peer.unblockWrites();
		await aborted;

		await waitFor(() => peer.has("reset_stream"));
		expect(await write).toBeInstanceOf(Error);
		const frames = peer.received().filter((f): f is Frame.Data => f.type === "stream");
		expect(frames.length).toBe(1);
		const reset = peer.received().find((f) => f.type === "reset_stream") as Frame.ResetStream;
		expect(reset.finalSize).toBe(BigInt(frames[0].data.byteLength));
		session.close();
	});

	test("application aborts do not log warnings", async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const { session, peer } = connect();

		try {
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			const uni = await session.createUnidirectionalStream();
			await uni.abort(new Error("application closed uni stream"));

			const bidi = await session.createBidirectionalStream();
			await bidi.writable.abort(new Error("application closed local bidi stream"));

			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Bi, true),
				data: new Uint8Array([1]),
				fin: false,
			});
			const incoming = await session.incomingBidirectionalStreams.getReader().read();
			if (!incoming.value) throw new Error("expected an incoming bidirectional stream");
			await incoming.value.writable.abort(new Error("application closed incoming bidi stream"));

			await waitFor(() => peer.count("reset_stream") === 3);
			expect(warn).not.toHaveBeenCalled();
		} finally {
			session.close();
			warn.mockRestore();
		}
	});

	test("cancelling with a WebTransportError sends its streamErrorCode as STOP_SENDING", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id, data: new Uint8Array([1, 2, 3]), fin: false });

		const reader = session.incomingUnidirectionalStreams.getReader();
		const incoming = await reader.read();
		if (!incoming.value) throw new Error("expected an incoming stream");
		await incoming.value.cancel(new StreamError(2, "STOP_SENDING"));

		await waitFor(() => peer.has("stop_sending"));
		const stop = peer.received().find((f) => f.type === "stop_sending") as Frame.StopSending;
		expect(stop.code.value).toBe(2n);
		session.close();
	});

	test("a received RESET_STREAM errors the readable with the peer's code", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id, data: new Uint8Array([1, 2, 3]), fin: false });

		const reader = session.incomingUnidirectionalStreams.getReader();
		const incoming = await reader.read();
		if (!incoming.value) throw new Error("expected an incoming stream");

		// Drain what arrived before the reset, so the next read is the one that observes it.
		const stream = incoming.value.getReader();
		expect((await stream.read()).value).toEqual(new Uint8Array([1, 2, 3]));

		peer.send({ type: "reset_stream", id, code: VarInt.from(31), finalSize: 3n });

		// The code is the point: an application distinguishing "cancelled, move on" from
		// "data is missing" reads it off the error rather than parsing the message.
		const err = await stream.read().then(
			() => undefined,
			(e: unknown) => e,
		);
		expect(err).toBeInstanceOf(StreamError);
		expect((err as StreamError).source).toBe("stream");
		expect((err as StreamError).streamErrorCode).toBe(31);
		session.close();
	});

	test("a received STOP_SENDING errors the writable with the peer's code", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const writable = await session.createUnidirectionalStream();
		const writer = writable.getWriter();
		await writer.write(new Uint8Array([1, 2, 3]));

		const id = Stream.Id.create(0n, Stream.Dir.Uni, false);
		peer.send({ type: "stop_sending", id, code: VarInt.from(2) });

		const err = await writer.closed.then(
			() => undefined,
			(e: unknown) => e,
		);
		expect(err).toBeInstanceOf(StreamError);
		expect((err as StreamError).streamErrorCode).toBe(2);
		session.close();
	});

	test("a RESET_STREAM received first keeps the stream terminal", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(5n, Stream.Dir.Uni, true);
		peer.send({ type: "reset_stream", id, code: VarInt.from(0), finalSize: 0n });
		peer.send({ type: "stream", id, data: new Uint8Array([1, 2, 3]), fin: false });
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));

		const reader = session.incomingUnidirectionalStreams.getReader();
		const next = reader.read();
		const resurrected = await Promise.race([
			next.then(() => true),
			new Promise<false>((resolve) => setTimeout(() => resolve(false), 50)),
		]);
		expect(resurrected).toBe(false);
		session.close();
		await next;
		reader.releaseLock();
	});

	test("opening a high stream index doesn't cost a slot per skipped stream", async () => {
		// The peer may open any index under its credit, implicitly opening every
		// lower one. Tracking each skipped index would let one tiny frame make us
		// allocate (and loop) once per stream below it.
		const { session, peer } = connect({ maxStreamsUni: 1n << 40n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const high = Stream.Id.create((1n << 40n) - 1n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id: high, data: new Uint8Array([1]), fin: true });
		// A skipped lower stream is still openable, and a finished one stays closed.
		const low = Stream.Id.create(7n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id: low, data: new Uint8Array([2]), fin: true });
		peer.send({ type: "stream", id: high, offset: 1n, data: new Uint8Array([3]), fin: false });
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));

		const acceptor = session.incomingUnidirectionalStreams.getReader();
		const first = await acceptor.read();
		const second = await acceptor.read();
		const firstData = await first.value?.getReader().read();
		const secondData = await second.value?.getReader().read();
		expect([firstData?.value?.[0], secondData?.value?.[0]]).toEqual([1, 2]);
		expect(peer.has("connection_close")).toBe(false);
		acceptor.releaseLock();
		session.close();
	});

	test("RESET_STREAM final_size consumes connection flow control", async () => {
		const { session, peer } = connect({ receiveWindow: 4n, streamReceiveWindow: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({
			type: "reset_stream",
			id: Stream.Id.create(0n, Stream.Dir.Uni, true),
			code: VarInt.from(0),
			finalSize: 5n,
		});

		const err = await expectSessionFailure(session);
		expect(err.message).toContain("flow control error");
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("RESET_STREAM final_size is cumulative with later stream data for MAX_DATA", async () => {
		const { session, peer } = connect({ receiveWindow: 10n, streamReceiveWindow: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// Three bytes do not cross the receive-window replenishment threshold. Eight
		// bytes on a second stream therefore exceed MAX_DATA (3 + 8 > 10), proving
		// the reset-only final size shares the cumulative connection budget.
		peer.send({
			type: "reset_stream",
			id: Stream.Id.create(0n, Stream.Dir.Uni, true),
			code: VarInt.from(0),
			finalSize: 3n,
		});
		peer.send({
			type: "stream",
			id: Stream.Id.create(1n, Stream.Dir.Uni, true),
			data: new Uint8Array(8),
			fin: false,
		});

		const err = await expectSessionFailure(session);
		expect(err.message).toContain("flow control error");
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("RESET_STREAM final_size replenishes connection credit once discarded", async () => {
		const { session, peer } = connect({ receiveWindow: 10n, streamReceiveWindow: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({
			type: "reset_stream",
			id: Stream.Id.create(0n, Stream.Dir.Uni, true),
			code: VarInt.from(0),
			finalSize: 6n,
		});

		await waitFor(() => peer.has("max_data"));
		const update = peer.received().find((frame) => frame.type === "max_data") as Frame.MaxData;
		expect(update.max).toBe(16n);
		session.close();
	});

	test("draft-01 tolerates the legacy zero final_size after data", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id, data: new Uint8Array([1, 2, 3]), fin: false });
		peer.send({ type: "reset_stream", id, code: VarInt.from(0), finalSize: 0n });
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));
		expect(peer.has("connection_close")).toBe(false);
		session.close();
	});

	test("sequential stream writes and FIN carry increasing OFF values", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const writable = await session.createUnidirectionalStream();
		const writer = writable.getWriter();
		await writer.write(new Uint8Array([1, 2, 3]));
		await writer.write(new Uint8Array([4, 5]));
		await writer.close();

		await waitFor(() => peer.received().filter((frame) => frame.type === "stream").length === 3);
		const streams = peer.received().filter((frame): frame is Frame.Data => frame.type === "stream");
		expect(streams.map((frame) => frame.offset)).toEqual([0n, 3n, 5n]);
		expect(streams.map((frame) => frame.fin)).toEqual([false, false, true]);

		const streamTypes = peer.sent.map((record) => record[0]).filter((type) => type >= 0x08 && type <= 0x0f);
		expect(streamTypes).toEqual([0x0e, 0x0e, 0x0f]);
		session.close();
	});

	test("contiguous receive offsets are delivered in order", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id, offset: 0n, data: new Uint8Array([1, 2]), fin: false });
		peer.send({ type: "stream", id, offset: 2n, data: new Uint8Array([3]), fin: true });

		const incoming = await session.incomingUnidirectionalStreams.getReader().read();
		if (incoming.done || !incoming.value) throw new Error("expected incoming stream");
		const reader = incoming.value.getReader();
		expect(Array.from((await reader.read()).value ?? [])).toEqual([1, 2]);
		expect(Array.from((await reader.read()).value ?? [])).toEqual([3]);
		expect((await reader.read()).done).toBe(true);
		session.close();
	});

	// The transport is ordered, so a frame that doesn't start where the last one
	// ended would deliver bytes out of order or twice: a protocol violation.
	for (const [name, offset] of [
		["a gap", 3n],
		["an overlap", 1n],
		["a duplicate", 0n],
	] as const) {
		test(`a STREAM frame with ${name} closes the session`, async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
			peer.send({ type: "stream", id, offset: 0n, data: new Uint8Array([1, 2]), fin: false });
			peer.send({ type: "stream", id, offset, data: new Uint8Array([3, 4]), fin: false });

			const err = await expectSessionFailure(session);
			expect((err.cause as Error).message).toContain("does not continue the stream");
			await waitFor(() => sentCloseCode(peer) === 1002);
		});
	}

	test("a FIN at the wrong offset closes the session", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id, offset: 0n, data: new Uint8Array([1, 2]), fin: false });
		peer.send({ type: "stream", id, offset: 5n, data: new Uint8Array(), fin: true });

		await expectSessionFailure(session);
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("a new stream must start at offset zero", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({
			type: "stream",
			id: Stream.Id.create(0n, Stream.Dir.Uni, true),
			offset: 4n,
			data: new Uint8Array([1]),
			fin: false,
		});

		await expectSessionFailure(session);
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("frames without OFF from a pre-offset sender continue the stream", async () => {
		// Senders released before offsets were encoded omit OFF on every frame,
		// which reads as offset zero. Their frames are contiguous by construction.
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// 0x0a = STREAM | LEN, 0x0b = STREAM | LEN | FIN; server-initiated uni id 3.
		peer.sendRaw(new Uint8Array([0x0a, 0x03, 0x02, 1, 2]));
		peer.sendRaw(new Uint8Array([0x0b, 0x03, 0x01, 3]));

		const incoming = await session.incomingUnidirectionalStreams.getReader().read();
		if (incoming.done || !incoming.value) throw new Error("expected incoming stream");
		const reader = incoming.value.getReader();
		expect(Array.from((await reader.read()).value ?? [])).toEqual([1, 2]);
		expect(Array.from((await reader.read()).value ?? [])).toEqual([3]);
		expect((await reader.read()).done).toBe(true);
		expect(peer.has("connection_close")).toBe(false);
		session.close();
	});

	test("serverCertificateHashes is refused rather than silently ignored", () => {
		(globalThis as { WebSocketStream?: unknown }).WebSocketStream = FakePeer;
		// Pinning can't be enforced over WebSocket; connecting anyway would drop the
		// authentication the caller asked for.
		expect(
			() =>
				new Session("https://example/test", {
					serverCertificateHashes: [{ algorithm: "sha-256", value: new Uint8Array(32) }],
				}),
		).toThrow("serverCertificateHashes");

		// An empty list pins nothing, so there's nothing to refuse.
		new Session("https://example/test", { serverCertificateHashes: [] }).close();
	});

	test("a peer that pings without reading gets only its newest ping answered", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });
		await waitFor(() => peer.has("transport_parameters"));

		// The peer stops reading but keeps pinging. Queuing a response per request
		// would grow our memory without bound.
		peer.blockWrites();
		for (let i = 1n; i <= 1000n; i++) peer.send({ type: "ping_request", sequence: i });
		await settle();
		peer.unblockWrites();

		const answered = () =>
			peer
				.received()
				.filter((f): f is Frame.PingResponse => f.type === "ping_response")
				.map((f) => f.sequence);
		await waitFor(() => answered().includes(1000n));
		await settle();
		// At most one response was already on its way into the wedged socket; the
		// rest collapsed into the newest.
		expect(answered().length).toBeLessThanOrEqual(2);
		expect(peer.has("connection_close")).toBe(false);
		session.close();
	});

	test("keep-alives don't pile up behind a wedged socket", async () => {
		const clock = new Manual();
		const installed = clock.install();
		try {
			const { session, peer } = connect({ idleTimeout: 300 });
			await session.ready;
			await waitFor(() => peer.has("transport_parameters"));
			peer.send({ type: "transport_parameters", params: peerParams() });
			await settle();

			// Our writes stall, but the peer keeps talking, so the session stays up and
			// every keep-alive tick finds our send side silent.
			peer.blockWrites();
			for (let i = 0; i < 40; i++) {
				await clock.advance(Nano.fromMilli(Milli(50)));
				peer.send({ type: "ping_response", sequence: 0n });
				await settle();
			}
			peer.unblockWrites();
			await waitFor(() => peer.has("ping_request"));
			await settle();

			expect(peer.count("ping_request")).toBeLessThanOrEqual(2);
			expect(peer.has("connection_close")).toBe(false);
			session.close();
		} finally {
			installed.close();
		}
	});

	test("malformed and unsupported config is refused before transport setup", () => {
		const socket = {
			opened: new Promise(() => {}),
			closed: new Promise(() => {}),
			close: () => {},
		} as unknown as WebSocketStreamLike;
		const invalid = [
			{ receiveWindow: -1 },
			{ receiveWindow: null },
			{ receiveWindow: "64" },
			{ streamReceiveWindow: 64 },
			{ maxStreamsBidi: null },
			{ maxStreamsUni: 1 },
			{ maxRecordSize: null },
			{ maxDatagramFrameSize: 64 },
			{ maxData: 64n },
			{ maxStreamDataUni: 64n },
			{ maxIdleTimeout: 1000n },
			{ unknown: 1n },
		];
		for (const config of invalid) {
			expect(() => Session.accept(socket, { config: config as unknown as Config })).toThrow();
		}
	});

	test("undefined optional settings resolve to defaults before advertisement", async () => {
		const { session, peer } = connect({
			receiveWindow: undefined,
			streamReceiveWindow: undefined,
			idleTimeout: undefined,
			maxDatagramFrameSize: undefined,
		});
		await session.ready;
		await waitFor(() => peer.has("transport_parameters"));
		const frame = peer
			.received()
			.find(
				(frame): frame is Frame.TransportParameters => frame.type === "transport_parameters",
			) as Frame.TransportParameters;
		expect(frame.params.initialMaxData).toBe(1_048_576n);
		expect(frame.params.initialMaxStreamDataUni).toBe(262_144n);
		expect(frame.params.maxIdleTimeout).toBe(30_000n);
		expect(frame.params.maxDatagramFrameSize).toBe(Frame.DEFAULT_MAX_RECORD_SIZE);
		session.close();
	});

	test("disabled config uses null and invalid durations fail before opening", () => {
		for (const idleTimeout of [0, -1, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
			expect(() => new Session("https://example/test", { config: { idleTimeout } })).toThrow("idleTimeout");
		}
		expect(() => new Session("https://example/test", { config: { maxDatagramFrameSize: 0n } })).toThrow(
			"maxDatagramFrameSize",
		);
	});

	test("config values that don't fit a varint are refused up front", () => {
		(globalThis as { WebSocketStream?: unknown }).WebSocketStream = FakePeer;
		expect(() => new Session("https://example/test", { config: { receiveWindow: -1n } })).toThrow("VarInt");
		expect(() => new Session("https://example/test", { config: { maxStreamsBidi: VarInt.MAX + 1n } })).toThrow(
			"VarInt",
		);
	});

	test("a failed write closes the session even if the socket never reports closing", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });
		await waitFor(() => peer.has("transport_parameters"));

		peer.failWrites(new Error("broken pipe"));
		peer.send({ type: "ping_request", sequence: 1n });

		const err = await expectSessionFailure(session);
		expect(err.message).toBe("broken pipe");
	});

	test("a STREAM frame on a bidi stream we never opened closes the session", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// Client-initiated bidi index 0: our role, but we never opened it.
		peer.send({
			type: "stream",
			id: Stream.Id.create(0n, Stream.Dir.Bi, false),
			data: new Uint8Array(8),
			fin: false,
		});

		const err = await expectSessionFailure(session);
		expect((err.cause as Error).message).toContain("never opened");
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("a RESET_STREAM on a stream we never opened closes the session", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({
			type: "reset_stream",
			id: Stream.Id.create(3n, Stream.Dir.Bi, false),
			code: VarInt.from(0),
			finalSize: 0n,
		});

		const err = await expectSessionFailure(session);
		expect((err.cause as Error).message).toContain("never opened");
	});

	test("a late STREAM frame on a bidi stream we opened and stopped reading is dropped", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const bidi = await session.createBidirectionalStream();
		await bidi.readable.cancel();
		await waitFor(() => peer.has("stop_sending"));

		peer.send({
			type: "stream",
			id: Stream.Id.create(0n, Stream.Dir.Bi, false),
			data: new Uint8Array(4),
			fin: false,
		});
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));
		expect(peer.has("connection_close")).toBe(false);
		session.close();
	});

	test("a peer bidi stream opened before params gets the peer's bidi_local credit", async () => {
		// qmux-01 doesn't require TRANSPORT_PARAMETERS first, so a peer may open a
		// stream before them. Its own bidi stream is limited by its bidi_local.
		const { session, peer } = connect();
		await session.ready;

		const id = Stream.Id.create(0n, Stream.Dir.Bi, true);
		peer.send({ type: "stream", id, offset: 0n, data: new Uint8Array([1]), fin: false });
		peer.send({
			type: "transport_parameters",
			params: peerParams({ initialMaxStreamDataBidiLocal: 10n, initialMaxStreamDataBidiRemote: 1000n }),
		});

		const acceptor = session.incomingBidirectionalStreams.getReader();
		const incoming = await acceptor.read();
		if (incoming.done || !incoming.value) throw new Error("expected an incoming stream");
		void incoming.value.writable
			.getWriter()
			.write(new Uint8Array(100))
			.catch(() => {});

		const sent = () =>
			peer
				.received()
				.filter((f): f is Frame.Data => f.type === "stream" && f.id.value.value === id.value.value)
				.reduce((n, f) => n + f.data.byteLength, 0);
		await waitFor(() => sent() > 0);
		await settle();
		expect(sent()).toBe(10);
		session.close();
	});

	test("a maxRecordSize below the protocol minimum is refused up front", () => {
		(globalThis as { WebSocketStream?: unknown }).WebSocketStream = FakePeer;
		expect(
			() => new Session("https://example/test", { config: { maxRecordSize: DEFAULT_MAX_RECORD_SIZE - 1n } }),
		).toThrow("maxRecordSize");
	});

	test("requireProtocol fails a handshake that picked no application protocol", async () => {
		(globalThis as { WebSocketStream?: unknown }).WebSocketStream = FakePeer;
		// The scripted server answers with the bare `qmux-01`, ignoring our pairs.
		const session = new Session("https://example/test", {
			protocols: ["moq-lite-04"],
			versions: { "moq-lite-04": "qmux-01" },
			requireProtocol: true,
			config: { idleTimeout: null },
		});

		const err = await settleTo(session.ready);
		expect((err as Error).message).toContain("no application protocol");
		await expectSessionFailure(session);
		expect(FakePeer.last?.closeInfo).toBeDefined();
	});

	test("STOP_SENDING fails a bidi write parked on stream credit", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamDataBidiRemote: 0n }) });

		const bidi = await session.createBidirectionalStream();
		const write = settleTo(bidi.writable.getWriter().write(new Uint8Array(10)));
		await settle();

		peer.send({ type: "stop_sending", id: Stream.Id.create(0n, Stream.Dir.Bi, false), code: VarInt.from(5) });
		const err = await Promise.race([write, new Promise((resolve) => setTimeout(() => resolve("pending"), 500))]);
		expect(err).toBeInstanceOf(StreamError);
		session.close();
	});

	test("no stream data follows STOP_SENDING once connection credit arrives", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams({ initialMaxData: 0n }) });

		const uni = await session.createUnidirectionalStream();
		const write = settleTo(uni.getWriter().write(new Uint8Array(10)));
		await settle();

		const id = Stream.Id.create(0n, Stream.Dir.Uni, false);
		peer.send({ type: "stop_sending", id, code: VarInt.from(5) });
		peer.send({ type: "max_data", max: 100n });
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));
		await settle();

		expect(await write).toBeInstanceOf(Error);
		expect(peer.received().some((f) => f.type === "stream" && f.id.value.value === id.value.value)).toBe(false);
		session.close();
	});

	test("close() sends APPLICATION_CLOSE, resolves closed, and is idempotent", async () => {
		const { session, peer } = connect();
		await session.ready;

		session.close({ closeCode: 42, reason: "bye" });
		expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
		await waitFor(() => peer.has("application_close"));
		// A graceful close is an APPLICATION_CLOSE (0x1d), so the peer fulfills.
		expect(sentClose(peer)?.type).toBe("application_close");

		// Second close is a no-op: the resolved info is unchanged.
		session.close({ closeCode: 7, reason: "again" });
		expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
	});

	test("a close before TRANSPORT_PARAMETERS is written still sends them first", async () => {
		const { session, peer } = connect();
		peer.blockWrites();
		await session.ready;

		session.close({ closeCode: 42, reason: "bye" });
		await settle();
		peer.unblockWrites();
		await waitFor(() => peer.has("application_close"));
		expect(peer.received().map((f) => f.type)).toEqual(["transport_parameters", "application_close"]);
	});

	// A close never waits on the peer to read: the socket closes once the close frame
	// is handed to it, or at the close deadline if the socket is still wedged then.
	describe("close behind a wedged socket", () => {
		let clock: Manual;
		let installed: Installed;
		beforeEach(() => {
			clock = new Manual();
			installed = clock.install();
		});
		afterEach(() => installed.close());

		/** Wedge the socket with a write in flight, a stream write and a RESET_STREAM
		 *  queued behind it, and return how many chunks reached the wire before. */
		async function wedge(session: Session, peer: FakePeer): Promise<number> {
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });
			await waitFor(() => peer.has("transport_parameters"));
			const writer = (await session.createUnidirectionalStream()).getWriter();
			const aborted = await session.createUnidirectionalStream();

			peer.blockWrites();
			peer.send({ type: "ping_request", sequence: 1n });
			await settle();
			void settleTo(writer.write(new Uint8Array(10)));
			await aborted.abort(new Error("cancel"));
			await settle();
			return peer.sent.length;
		}

		test("a socket that drains before the deadline delivers APPLICATION_CLOSE first", async () => {
			const { session, peer } = connect();
			const before = await wedge(session, peer);

			session.close({ closeCode: 42, reason: "bye" });
			await clock.advance(Nano.fromMilli(Milli(1000)));
			await settle();
			expect(peer.closeInfo).toBeUndefined();

			peer.unblockWrites();
			await waitFor(() => peer.closeInfo !== undefined);
			// Nothing queued behind the wedge goes out ahead of the close.
			const after = peer.sent.slice(before).flatMap((b) => Frame.decodeRecord(b));
			expect(after.map((f) => f.type)).toEqual(["ping_response", "application_close"]);
			expect(peer.sentAtClose).toBe(peer.sent.length);
		});

		test("a socket still wedged at the deadline closes without it", async () => {
			const { session, peer } = connect();
			await wedge(session, peer);

			session.close({ closeCode: 42, reason: "bye" });
			await clock.advance(Nano.fromMilli(Milli(2999)));
			await settle();
			expect(peer.closeInfo).toBeUndefined();

			await clock.advance(Nano.fromMilli(Milli(1)));
			await settle();
			expect(peer.closeInfo).toBeDefined();
			expect(peer.has("application_close")).toBe(false);
		});
	});

	// The WebTransport contract: `closed` fulfills only on a graceful end, and rejects
	// with a WebTransportError (source "session") on an abnormal one. Without that
	// split a consumer cannot tell a dropped session from a clean shutdown — the close
	// code can't carry it, since codes are application-defined.
	describe("closed settles graceful vs abnormal", () => {
		test("a peer APPLICATION_CLOSE resolves closed with its code and reason", async () => {
			const { session, peer } = connect();
			await session.ready;

			// The peer closing the session deliberately (APPLICATION_CLOSE / 0x1d) is
			// graceful, even though we didn't initiate it.
			peer.send({ type: "application_close", code: VarInt.from(42), reason: "bye" });
			expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
		});

		test("a peer APPLICATION_CLOSE fulfills even when its code looks like a transport error", async () => {
			const { session, peer } = connect();
			await session.ready;

			// 1002 is a transport-error code, but an APPLICATION_CLOSE is graceful by
			// definition — the *subtype*, not the code, decides. Codes are
			// application-defined, so the app still gets its code and reason.
			peer.send({ type: "application_close", code: VarInt.from(1002), reason: "bye" });
			expect(await session.closed).toEqual({ closeCode: 1002, reason: "bye" });
		});

		test("a peer CONNECTION_CLOSE rejects closed", async () => {
			const { session, peer } = connect();
			await session.ready;

			// The peer detected a protocol violation / transport error and sent a
			// CONNECTION_CLOSE (0x1c) — abnormal, so `closed` rejects rather than
			// fulfilling. Mirrors the frame we emit from #abort.
			peer.send({
				type: "connection_close",
				code: VarInt.from(1002),
				reason: "Protocol violation",
			});
			const err = await expectSessionFailure(session);
			expect(err.message).toContain("Protocol violation");
		});

		test("a socket drop with no CONNECTION_CLOSE rejects closed", async () => {
			const { session, peer } = connect();
			await session.ready;

			// Even a "clean" WebSocket status code is a drop at the session layer: the
			// peer never said goodbye in QMux terms.
			peer.close({ closeCode: 1000, reason: "" });

			const err = await expectSessionFailure(session);
			expect(err.message).toContain("Connection closed");
		});

		test("a socket error rejects closed, preserving the underlying failure", async () => {
			const { session, peer } = connect();
			await session.ready;

			peer.error(new Error("connection reset"));

			// Why the transport died is the whole value of the rejection, so the socket's
			// own error has to survive rather than be flattened into a fixed string.
			const err = await expectSessionFailure(session);
			expect(err.message).toBe("connection reset");
			expect((err.cause as Error).message).toBe("connection reset");
		});

		test("an idle timeout rejects closed when neither direction makes progress", async () => {
			// The sharpest case: an idle timeout used to resolve with { closeCode: 0,
			// reason: "idle timeout" }, while a graceful close() with no args resolves
			// with { closeCode: 0, reason: "" }. A dead peer and a clean shutdown differed
			// only by a free-text string.
			const clock = new Manual();
			const installed = clock.install();
			try {
				const { session, peer } = connect({ idleTimeout: 150 });
				await session.ready;
				await waitFor(() => peer.has("transport_parameters"));
				// A successfully written keep-alive is send activity under QMux. Block the
				// sink so neither inbound nor outbound frames can reset the idle deadline.
				peer.blockWrites();
				peer.send({ type: "transport_parameters", params: peerParams() });
				await settle();

				let settled = false;
				session.closed.then(
					() => {
						settled = true;
					},
					() => {
						settled = true;
					},
				);

				// Exactly at the deadline the session is still open; one tick past it closes.
				await clock.advance(Nano.fromMilli(Milli(150)));
				await settle();
				expect(settled).toBe(false);
				await clock.advance(Nano.fromMilli(Milli(50)));

				const err = await expectSessionFailure(session);
				expect(err.message).toContain("idle timeout");
				peer.unblockWrites();
			} finally {
				installed.close();
			}
		});

		test("a peer that never sends its parameters still idles out", async () => {
			const clock = new Manual();
			const installed = clock.install();
			try {
				const { session, peer } = connect({ idleTimeout: 150 });
				await session.ready;
				await waitFor(() => peer.has("transport_parameters"));

				await clock.advance(Nano.fromMilli(Milli(200)));
				const err = await expectSessionFailure(session);
				expect(err.message).toContain("idle timeout");
				expect(peer.has("ping_request")).toBe(false);
			} finally {
				installed.close();
			}
		});

		test("outbound frame activity resets the idle timeout", async () => {
			const clock = new Manual();
			const installed = clock.install();
			try {
				const { session, peer } = connect({ idleTimeout: 120 });
				await session.ready;
				peer.send({ type: "transport_parameters", params: peerParams() });

				const writable = await session.createUnidirectionalStream();
				const writer = writable.getWriter();
				for (let i = 0; i < 6; i++) {
					await clock.advance(Nano.fromMilli(Milli(45)));
					await writer.write(new Uint8Array([i]));
					// The peer answers every other write, as a live peer responding to the
					// keep-alive would. A send restarts the deadline only once per receive
					// (RFC 9000 §10.1), so proof of life has to keep arriving.
					if (i % 2 === 0) peer.send({ type: "ping_response", sequence: BigInt(i) });
					await settle();
				}

				// More than two idle windows elapsed with only occasional inbound frames,
				// but the successful STREAM writes kept resetting the shared deadline.
				expect(peer.has("connection_close")).toBe(false);
				session.close();
				expect(await session.closed).toEqual({ closeCode: 0, reason: "" });
			} finally {
				installed.close();
			}
		});

		test("a silent peer idle-closes even while our own writes land", async () => {
			// The dead-peer case: nothing ever arrives again, but the socket still
			// accepts everything we send. Our own keep-alive pings advance the send
			// clock, so counting every send as activity left the deadline permanently
			// in the future and the session never closed.
			const clock = new Manual();
			const installed = clock.install();
			try {
				const { session, peer } = connect({ idleTimeout: 120 });
				await session.ready;
				peer.send({ type: "transport_parameters", params: peerParams() });

				const writable = await session.createUnidirectionalStream();
				const writer = writable.getWriter();
				try {
					for (let i = 0; i < 12; i++) {
						await clock.advance(Nano.fromMilli(Milli(45)));
						await writer.write(new Uint8Array([i]));
						await settle();
					}
				} catch {
					// The session closes out from under the writer; that's the point.
				}

				const err = await expectSessionFailure(session);
				expect(err.message).toContain("idle timeout");
			} finally {
				installed.close();
			}
		});

		test("the standard reconnect idiom sees a drop", async () => {
			// The downstream bug this fixes (moq-dev/moq#2183): code written against the
			// native API reconnects from the catch arm, which qmux could never reach.
			const { session, peer } = connect();
			await session.ready;

			let reconnected = false;
			const watch = (async () => {
				try {
					await session.closed;
				} catch {
					reconnected = true;
				}
			})();

			peer.close({ closeCode: 1006, reason: "relay bounced" });
			await watch;

			expect(reconnected).toBe(true);
		});
	});

	test("a text frame rejects closed and tells the peer why (1003)", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.sendText("not binary");

		const err = await expectSessionFailure(session);
		expect(err.message).toContain("text frames are not valid for QMux");
		await waitFor(() => sentCloseCode(peer) === 1003);
		// A violation we detect is a CONNECTION_CLOSE (0x1c), so the peer rejects too.
		expect(sentClose(peer)?.type).toBe("connection_close");
	});

	test("an oversized WebSocket record rejects closed before frame decoding", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// An all-zero record is otherwise valid PADDING, so only the negotiated
		// record-size limit should make this fatal.
		peer.sendRaw(new Uint8Array(Number(DEFAULT_MAX_RECORD_SIZE) + 1));

		await waitFor(() => peer.has("connection_close"));
		await expectSessionFailure(session);
		expect(sentCloseCode(peer)).toBe(1002);
	});

	test("draft-01 rejects max_record_size below the default minimum", async () => {
		const { session, peer } = connect();
		await session.ready;

		peer.send({
			type: "transport_parameters",
			params: peerParams({ maxRecordSize: DEFAULT_MAX_RECORD_SIZE - 1n }),
		});

		await waitFor(() => peer.has("connection_close"));
		await expectSessionFailure(session);
		expect(sentCloseCode(peer)).toBe(1002);
	});

	test("an unnegotiated RESET_STREAM_AT rejects closed and tells the peer why (1002)", async () => {
		// The peer negotiates qmux-01, which never advertises `reset_stream_at`, so
		// a RESET_STREAM_AT (0x24) frame is a protocol violation. Hand-built bytes:
		// type=0x24, id=3, code=0, final_size=0, reliable_size=0.
		const { session, peer } = connect();
		await session.ready;
		peer.sendRaw(new Uint8Array([0x24, 0x03, 0x00, 0x00, 0x00]));

		const err = await expectSessionFailure(session);
		// The decode failure itself is carried as the cause, not flattened away.
		expect(err.cause).toBeInstanceOf(Error);
		await waitFor(() => sentCloseCode(peer) === 1002);
		expect(sentClose(peer)?.type).toBe("connection_close");
	});

	test("datagrams: lowering the live high-water mark retains backlog and sheds later overflow", async () => {
		const { session, peer } = connect();
		await session.ready;
		const datagrams = session.datagrams;
		datagrams.incomingHighWaterMark = 3;
		for (const value of [1, 2]) peer.send({ type: "datagram", data: new Uint8Array([value]) });
		await peer.barrier();

		datagrams.incomingHighWaterMark = 1;
		peer.send({ type: "datagram", data: new Uint8Array([3]) });
		await peer.barrier();
		const reader = datagrams.readable.getReader();
		expect((await reader.read()).value).toEqual(new Uint8Array([1]));
		peer.send({ type: "datagram", data: new Uint8Array([4]) });
		await peer.barrier();
		expect((await reader.read()).value).toEqual(new Uint8Array([2]));
		peer.send({ type: "datagram", data: new Uint8Array([5]) });
		await peer.barrier();
		session.close();
		expect((await reader.read()).value).toEqual(new Uint8Array([5]));
		expect((await reader.read()).done).toBe(true);
	});

	test("datagrams: raising the live high-water mark admits more queued packets", async () => {
		const { session, peer } = connect();
		await session.ready;
		session.datagrams.incomingHighWaterMark = 1;
		for (const value of [1, 2]) peer.send({ type: "datagram", data: new Uint8Array([value]) });
		await peer.barrier();
		session.datagrams.incomingHighWaterMark = 3;
		for (const value of [3, 4, 5]) peer.send({ type: "datagram", data: new Uint8Array([value]) });
		await peer.barrier();
		session.close();
		const received: number[] = [];
		for await (const data of session.datagrams.readable) received.push(data[0]);
		expect(received).toEqual([1, 3, 4]);
	});

	test("datagrams: the live limit validates input without changing the previous policy", async () => {
		const { session } = connect();
		await session.ready;
		const datagrams = session.datagrams;
		for (const value of [0, -0, 0.5, 1.5, Infinity]) {
			datagrams.incomingHighWaterMark = value;
			expect(datagrams.incomingHighWaterMark).toBe(Math.max(1, value));
		}
		datagrams.incomingHighWaterMark = 2;
		for (const value of [NaN, -1, -Infinity]) {
			expect(() => {
				datagrams.incomingHighWaterMark = value;
			}).toThrow(RangeError);
			expect(datagrams.incomingHighWaterMark).toBe(2);
		}
		for (const value of [undefined, null, "2", {}, 2n]) {
			expect(() => {
				Reflect.set(datagrams, "incomingHighWaterMark", value);
			}).toThrow(TypeError);
			expect(datagrams.incomingHighWaterMark).toBe(2);
		}
		session.close();
	});

	test("datagrams: unsupported policies accept only their defaults", async () => {
		const { session } = connect();
		await session.ready;
		const datagrams = session.datagrams;
		for (const [field, defaultValue] of [
			["incomingMaxAge", null],
			["outgoingMaxAge", null],
			["outgoingHighWaterMark", 1024],
		] as const) {
			expect(Reflect.set(datagrams, field, defaultValue)).toBe(true);
			for (const value of [0, 1, -1, NaN, Infinity, undefined, "1024"]) {
				try {
					Reflect.set(datagrams, field, value);
					throw new Error("unsupported datagram policy was accepted");
				} catch (err) {
					expect(err).toBeInstanceOf(DOMException);
					expect((err as DOMException).name).toBe("NotSupportedError");
				}
				expect(datagrams[field]).toBe(defaultValue);
			}
		}
		session.close();
	});

	test("datagrams: pending reads bypass the queue and draining frees capacity", async () => {
		const { session, peer } = connect();
		await session.ready;
		session.datagrams.incomingHighWaterMark = 1;
		const reader = session.datagrams.readable.getReader();
		const pending = reader.read();
		for (const value of [1, 2, 3]) peer.send({ type: "datagram", data: new Uint8Array([value]) });
		await peer.barrier();
		expect((await pending).value).toEqual(new Uint8Array([1]));
		expect((await reader.read()).value).toEqual(new Uint8Array([2]));
		peer.send({ type: "datagram", data: new Uint8Array([4]) });
		await peer.barrier();
		expect((await reader.read()).value).toEqual(new Uint8Array([4]));
		session.close();
		expect((await reader.read()).done).toBe(true);
	});

	for (const queued of [false, true]) {
		test(`datagrams: cancellation clears ${queued ? "queued packets" : "a pending read"} and ignores later arrivals`, async () => {
			const { session, peer } = connect();
			await session.ready;
			session.datagrams.incomingHighWaterMark = 1;
			if (queued) {
				peer.send({ type: "datagram", data: new Uint8Array([1]) });
				await peer.barrier();
			}
			const reader = session.datagrams.readable.getReader();
			const pending = queued ? undefined : reader.read();
			await reader.cancel();
			if (pending) expect((await pending).done).toBe(true);
			session.datagrams.incomingHighWaterMark = 2;
			peer.send({ type: "datagram", data: new Uint8Array([2]) });
			await peer.barrier();
			expect((await reader.read()).done).toBe(true);
			session.close();
			await session.closed;
		});
	}

	test("datagrams: session failure discards queued packets and rejects reads", async () => {
		const { session, peer } = connect();
		await session.ready;
		session.datagrams.incomingHighWaterMark = 1;
		peer.send({ type: "datagram", data: new Uint8Array([1]) });
		await peer.barrier();
		peer.error();
		await expectSessionFailure(session);
		await expect(session.datagrams.readable.getReader().read()).rejects.toThrow("socket error");
	});

	test("datagrams: the runtime view has no constructor or transport mutators for applications", async () => {
		const { session } = connect();
		await session.ready;
		const datagrams = session.datagrams;
		expect("Datagrams" in QMux).toBe(false);
		for (const key of ["push", "close", "setMaxDatagramSize"]) expect(key in datagrams).toBe(false);
		expect(() => Reflect.construct(datagrams.constructor, [() => {}])).toThrow(TypeError);
		expect(Reflect.set(datagrams, "maxDatagramSize", 100)).toBe(false);
		expect(Reflect.set(session, "datagrams", {})).toBe(false);
		session.close();
	});

	test("datagrams: an app write produces a DATAGRAM frame on the wire", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		await waitFor(() => session.datagrams.maxDatagramSize > 0);
		await session.datagrams.writable.getWriter().write(new Uint8Array([1, 2, 3]));

		await waitFor(() => peer.has("datagram"));
		const dg = peer.received().find((f) => f.type === "datagram") as Frame.Datagram;
		expect(Array.from(dg.data)).toEqual([1, 2, 3]);
		session.close();
	});

	test("datagrams: an incoming DATAGRAM frame is delivered to the readable", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array([9, 8, 7]) });
		const { value } = await session.datagrams.readable.getReader().read();
		expect(Array.from(value as Uint8Array)).toEqual([9, 8, 7]);
		session.close();
	});

	test("native WebSocketStream ArrayBuffer records are normalized before QMux decoding", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.sendArrayBuffer({ type: "datagram", data: new Uint8Array([4, 5, 6]) });
		const { value } = await session.datagrams.readable.getReader().read();
		expect(Array.from(value as Uint8Array)).toEqual([4, 5, 6]);
		session.close();
	});

	test("datagrams: a DATAGRAM whose frame exactly fits our advertised size is delivered", async () => {
		// ourParams.maxDatagramFrameSize = 10; an 8-byte payload encodes to a
		// 1 (type) + 1 (length varint) + 8 = 10-byte frame — exactly the limit.
		const { session, peer } = connect({ maxDatagramFrameSize: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]) });
		const { value } = await session.datagrams.readable.getReader().read();
		expect(Array.from(value as Uint8Array)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
		session.close();
	});

	test("datagrams: a DATAGRAM whose frame overflows our advertised size is fatal", async () => {
		// ourParams.maxDatagramFrameSize = 10. A 10-byte payload encodes to a
		// 1 + 1 + 10 = 12-byte frame > 10 — a peer exceeding the size we negotiated
		// is a PROTOCOL_VIOLATION (RFC 9221), so the session must close, not drop.
		const { session, peer } = connect({ maxDatagramFrameSize: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array(10) });
		await expectSessionFailure(session);
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("datagrams: a DATAGRAM we never advertised support for is fatal", async () => {
		// ourParams.maxDatagramFrameSize = 0 → we advertised no datagram support, so
		// receiving one at all is a PROTOCOL_VIOLATION (RFC 9221), not a droppable frame.
		const { session, peer } = connect({ maxDatagramFrameSize: null });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({ type: "datagram", data: new Uint8Array([1, 2, 3]) });
		await expectSessionFailure(session);
		await waitFor(() => sentCloseCode(peer) === 1002);
	});

	test("datagrams: a no-length (0x30) DATAGRAM is sized without a length varint", async () => {
		// ourParams.maxDatagramFrameSize = 10. A 0x30 datagram carries no length
		// varint, so a 9-byte payload is a 1 + 9 = 10-byte frame — exactly the
		// limit — even though the length-prefixed reconstruction (1 + 1 + 9 = 11)
		// would wrongly drop it. Hand-build the record: a 0x30 type byte + payload.
		const { session, peer } = connect({ maxDatagramFrameSize: 10n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const payload = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9]);
		peer.sendRaw(new Uint8Array([0x30, ...payload]));
		const { value } = await session.datagrams.readable.getReader().read();
		expect(Array.from(value as Uint8Array)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9]);
		session.close();
	});

	test("datagrams: maxDatagramSize reflects the peer's advertised frame size", async () => {
		const { session, peer } = connect();
		await session.ready;
		// frame size 1201 → payload 1201 - (1 type byte + 2-byte length varint) = 1198.
		peer.send({ type: "transport_parameters", params: peerParams({ maxDatagramFrameSize: 1201n }) });

		await waitFor(() => session.datagrams.maxDatagramSize > 0);
		expect(session.datagrams.maxDatagramSize).toBe(1198);
		session.close();
	});

	test("datagrams: a peer that disables datagrams leaves maxDatagramSize at 0 and drops writes", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams({ maxDatagramFrameSize: 0n }) });

		// A ping barrier confirms the params have been processed.
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));

		expect(session.datagrams.maxDatagramSize).toBe(0);

		// Writing is dropped, not errored: no DATAGRAM frame reaches the wire.
		await session.datagrams.writable.getWriter().write(new Uint8Array([1, 2, 3]));
		peer.send({ type: "ping_request", sequence: 2n });
		await waitFor(() => peer.count("ping_response") === 2);
		expect(peer.has("datagram")).toBe(false);
		session.close();
	});

	test("datagrams: readable closes cleanly on a graceful session close", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const reader = session.datagrams.readable.getReader();
		session.close(); // graceful: no #closeReason, so the readable must not error
		const { done } = await reader.read();
		expect(done).toBe(true);
	});

	test("MAX_STREAM_DATA is extended on delivery to the app, not on receipt", async () => {
		// Small per-stream window so a few delivered chunks cross the half-window threshold.
		const { session, peer } = connect({ streamReceiveWindow: 1000n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// Peer opens a server-initiated uni stream and sends 4×200B (=800 ≤ window).
		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		for (let i = 0; i < 4; i++) {
			peer.send({ type: "stream", id, offset: BigInt(i * 200), data: new Uint8Array(200), fin: false });
		}
		// A ping after the data acts as a barrier: the Session processes frames in
		// order, so once we see the PONG, all four STREAM frames have been received.
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));

		// All bytes are received but not yet delivered to the app (only the eagerly
		// pulled first chunk, 200B < half the window). Credit-on-receipt would have
		// already emitted MAX_STREAM_DATA here; credit-on-delivery has not.
		expect(peer.count("max_stream_data")).toBe(0);

		// App takes the incoming stream and drains it.
		const reader = session.incomingUnidirectionalStreams.getReader();
		const { value: incoming, done } = await reader.read();
		reader.releaseLock();
		if (done || !incoming) throw new Error("expected an incoming unidirectional stream");

		const streamReader = incoming.getReader();
		let got = 0;
		while (got < 800) {
			const chunk = await streamReader.read();
			if (chunk.done) break;
			got += chunk.value.byteLength;
		}
		expect(got).toBe(800);

		// Delivering the buffered bytes replenishes the window.
		await waitFor(() => peer.count("max_stream_data") > 0);
		session.close();
	});

	test("completed unaccepted streams do not recycle receive credit", async () => {
		// A single-stream/single-payload budget makes any premature replenishment
		// immediately observable. This is the reproduction from issue #299: the peer
		// sends a full stream plus FIN while the application never reads the acceptor.
		const { session, peer } = connect({
			maxStreamsUni: 1n,
			receiveWindow: 100n,
			streamReceiveWindow: 100n,
		});
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.send({
			type: "stream",
			id: Stream.Id.create(0n, Stream.Dir.Uni, true),
			data: new Uint8Array(100),
			fin: true,
		});
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));

		// FIN only completed the wire-level receive. The stream object and all 100
		// bytes are still transport-owned, so neither budget may be recycled yet.
		expect(peer.count("max_streams_uni")).toBe(0);
		expect(peer.count("max_data")).toBe(0);
		expect(peer.count("max_stream_data")).toBe(0);

		const acceptor = session.incomingUnidirectionalStreams.getReader();
		const accepted = await acceptor.read();
		if (accepted.done || !accepted.value) throw new Error("expected an incoming stream");

		// Accepting transfers ownership of the stream object, but the unread payload
		// remains charged to MAX_DATA until the application asks for it.
		await waitFor(() => peer.count("max_streams_uni") === 1);
		expect(peer.count("max_data")).toBe(0);

		const payload = await accepted.value.getReader().read();
		expect(payload.value?.byteLength).toBe(100);
		await waitFor(() => peer.count("max_data") === 1);
		session.close();
	});

	test("accepted active streams do not recycle stream-count credit", async () => {
		const { session, peer } = connect({ maxStreamsUni: 1n });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		peer.send({ type: "stream", id, data: new Uint8Array([1]), fin: false });

		const acceptor = session.incomingUnidirectionalStreams.getReader();
		const accepted = await acceptor.read();
		if (accepted.done || !accepted.value) throw new Error("expected an incoming stream");

		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));
		expect(peer.count("max_streams_uni")).toBe(0);

		peer.send({ type: "stream", id, offset: 1n, data: new Uint8Array(), fin: true });
		await waitFor(() => peer.count("max_streams_uni") === 1);
		session.close();
	});

	// Stream credit bounds open streams only if a bidi stream holds it until both halves end.
	describe("a half-closed bidi stream keeps its stream-count credit", () => {
		const id = Stream.Id.create(0n, Stream.Dir.Bi, true);

		async function accept() {
			const { session, peer } = connect({ maxStreamsBidi: 1n });
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });
			peer.send({ type: "stream", id, data: new Uint8Array([1]), fin: false });
			const accepted = await session.incomingBidirectionalStreams.getReader().read();
			if (accepted.done || !accepted.value) throw new Error("expected an incoming stream");
			return { session, peer, stream: accepted.value };
		}

		// A ping round trip flushes any MAX_STREAMS the frames before it produced.
		async function expectCredit(peer: FakePeer, count: number) {
			const pongs = peer.count("ping_response");
			peer.send({ type: "ping_request", sequence: BigInt(pongs + 1) });
			await waitFor(() => peer.count("ping_response") > pongs);
			expect(peer.count("max_streams_bidi")).toBe(count);
		}

		test("until our send half finishes", async () => {
			const { session, peer, stream } = await accept();
			peer.send({ type: "stream", id, offset: 1n, data: new Uint8Array(), fin: true });
			await expectCredit(peer, 0);

			await stream.writable.close();
			await waitFor(() => peer.count("max_streams_bidi") === 1);
			session.close();
		});

		test("until the peer's send half finishes", async () => {
			const { session, peer, stream } = await accept();
			await stream.writable.close();
			await expectCredit(peer, 0);

			peer.send({ type: "stream", id, offset: 1n, data: new Uint8Array(), fin: true });
			await waitFor(() => peer.count("max_streams_bidi") === 1);
			session.close();
		});

		test("until our send half is reset", async () => {
			const { session, peer, stream } = await accept();
			await stream.readable.cancel();
			await expectCredit(peer, 0);

			await stream.writable.abort();
			await waitFor(() => peer.count("max_streams_bidi") === 1);
			session.close();
		});

		test("until the peer stops our send half", async () => {
			const { session, peer } = await accept();
			peer.send({ type: "reset_stream", id, code: VarInt.from(0), finalSize: 1n });
			await expectCredit(peer, 0);

			peer.send({ type: "stop_sending", id, code: VarInt.from(0) });
			await waitFor(() => peer.count("max_streams_bidi") === 1);
			session.close();
		});
	});

	// Regression: a STREAM frame delivered by the read loop after the session has
	// closed used to hit the already-closed incoming-stream controllers, and the
	// throw escaped as an unhandled rejection because #handleStreamFrame was an
	// unawaited async call. These tests capture process-level unhandled rejections
	// so the failure mode is asserted, not just observed in logs.
	describe("STREAM frame vs session teardown", () => {
		const rejections: unknown[] = [];
		const onRejection = (err: unknown) => {
			rejections.push(err);
		};

		beforeEach(async () => {
			process.on("unhandledRejection", onRejection);
			// Drain any late async work from earlier tests before arming the capture,
			// so a stray rejection from another test's teardown can't be misattributed
			// to the test about to run.
			await settle();
			rejections.length = 0;
		});
		afterEach(() => {
			process.off("unhandledRejection", onRejection);
		});

		test("a STREAM frame arriving after close() is dropped without an unhandled rejection", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			session.close({ closeCode: 42, reason: "bye" });

			// Peer-initiated streams still in flight when the session closed: one uni,
			// one bidi — both would hit the now-closed incoming-stream controllers.
			// The third targets a stream id that could only exist pre-close (#recvStreams
			// was cleared by close), so it also takes the new-stream branch.
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Uni, true),
				data: new Uint8Array(3),
				fin: false,
			});
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Bi, true),
				data: new Uint8Array(3),
				fin: false,
			});
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Uni, true),
				data: new Uint8Array(3),
				fin: true,
			});

			await settle();
			expect(rejections).toEqual([]);
			expect(await session.closed).toEqual({ closeCode: 42, reason: "bye" });
		});

		test("a STREAM frame on a send-only stream ID fails the session with 1002", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			// A client-initiated uni stream is send-only from our (client) perspective;
			// the peer writing to it is a protocol violation. Now that #handleStreamFrame
			// is synchronous, the throw propagates to #onData and closes with 1002 —
			// previously it leaked as a rejection while the session limped on.
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Uni, false),
				data: new Uint8Array([1]),
				fin: false,
			});

			await expectSessionFailure(session);
			await waitFor(() => sentCloseCode(peer) === 1002);
			await settle();
			expect(rejections).toEqual([]);
		});

		test("a stream arriving after the app cancelled the acceptor is refused, not fatal", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			// App stops accepting incoming uni streams while the session stays open.
			await session.incomingUnidirectionalStreams.cancel();
			peer.send({
				type: "stream",
				id: Stream.Id.create(0n, Stream.Dir.Uni, true),
				data: new Uint8Array(3),
				fin: false,
			});

			// Ping barrier: once the pong is seen the STREAM frame above has been fully
			// processed. The session must still be alive — an app-local cancel is not a
			// peer protocol violation.
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"), 5000);

			await settle();
			expect(rejections).toEqual([]);
			expect(peer.has("connection_close")).toBe(false);
			session.close();
		});

		test("repeated frames on a refused stream id do not amplify STOP_SENDING or re-credit stream count", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			await session.incomingUnidirectionalStreams.cancel();

			// One peer-initiated uni stream, its payload split across several in-flight
			// STREAM frames (all before the peer could honor any STOP_SENDING). Deleting
			// the stream on refusal would make each later frame re-open it, re-crediting
			// MAX_STREAMS and emitting a STOP_SENDING per frame; keeping it registered
			// means exactly one refusal.
			const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
			for (let i = 0; i < 8; i++) {
				peer.send({ type: "stream", id, offset: BigInt(i * 3), data: new Uint8Array(3), fin: false });
			}
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"), 5000);

			await settle();
			expect(rejections).toEqual([]);
			// No per-frame amplification: the refused stream never sends STOP_SENDING and
			// never re-credits the peer's stream-count limit.
			expect(peer.count("stop_sending")).toBe(0);
			expect(peer.has("max_streams_uni")).toBe(false);
			expect(peer.has("connection_close")).toBe(false);
			session.close();
		});

		test("refused streams do not leak connection flow-control credit", async () => {
			// Tight connection window (300B). Each refused stream's bytes advance the
			// connection recv offset; if they aren't also counted as consumed, the
			// advertised window never replenishes and the peer is wrongly starved /
			// eventually tripped into a 1002 flow-control close.
			const { session, peer } = connect({ receiveWindow: 300n });
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams() });

			await session.incomingUnidirectionalStreams.cancel();

			// Five distinct refused uni streams carrying 500B total — well past the 300B
			// window. With correct accounting the window keeps pace (MAX_DATA is emitted)
			// and the session stays healthy; the buggy refuse path froze `consumed` and
			// closed with a spurious flow-control error by the 4th stream.
			for (let i = 0n; i < 5n; i++) {
				peer.send({
					type: "stream",
					id: Stream.Id.create(i, Stream.Dir.Uni, true),
					data: new Uint8Array(100),
					fin: false,
				});
			}
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"), 5000);

			await settle();
			expect(rejections).toEqual([]);
			expect(peer.has("connection_close")).toBe(false);
			// The connection window advanced, proving refused bytes were credited back.
			expect(peer.has("max_data")).toBe(true);
			session.close();
		});
	});

	// Chrome rejects an over-limit create rather than queueing it, and @moq/net
	// passes waitUntilAvailable: false where it relies on that.
	describe("waitUntilAvailable: false refuses instead of waiting", () => {
		const refused = (err: unknown) => {
			expect(err).toBeInstanceOf(DOMException);
			expect((err as DOMException).name).toBe("NetworkError");
		};

		test("an exhausted uni limit rejects at once, and a later grant is usable", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsUni: 1n }) });

			await session.createUnidirectionalStream({ waitUntilAvailable: false });
			refused(await settleTo(session.createUnidirectionalStream({ waitUntilAvailable: false })));

			// The refusal claimed nothing: one more slot opens exactly one more stream.
			peer.send({ type: "max_streams_uni", max: 2n });
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"));
			await session.createUnidirectionalStream({ waitUntilAvailable: false });
			refused(await settleTo(session.createUnidirectionalStream({ waitUntilAvailable: false })));
			session.close();
		});

		test("an exhausted bidi limit rejects at once", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsBidi: 0n }) });

			refused(await settleTo(session.createBidirectionalStream({ waitUntilAvailable: false })));
			session.close();
		});

		test("a create before TRANSPORT_PARAMETERS waits for the peer's limit", async () => {
			const { session, peer } = connect();
			await session.ready;

			const created = session.createUnidirectionalStream({ waitUntilAvailable: false });
			await flushMicrotasks();
			peer.send({ type: "transport_parameters", params: peerParams() });

			expect(await created).toBeInstanceOf(WritableStream);
			session.close();
		});

		test("a create before TRANSPORT_PARAMETERS rejects with the close reason", async () => {
			const { session } = connect();
			await session.ready;

			const created = settleTo(session.createBidirectionalStream({ waitUntilAvailable: false }));
			await flushMicrotasks();
			session.close({ closeCode: 42, reason: "bye" });

			expect(((await created) as Error).message).toBe("Connection closed: 42 bye");
		});
	});

	describe("close threads its reason into blocked Credit.claim() waiters", () => {
		test("a uni stream blocked on stream-count credit rejects with the graceful close reason", async () => {
			const { session, peer } = connect();
			await session.ready;
			// Peer grants zero uni stream-count credit, so createUnidirectionalStream parks on claim().
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsUni: 0n }) });
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"));

			const created = settleTo(session.createUnidirectionalStream());
			await flushMicrotasks(); // let the call reach its parked claim()

			session.close({ closeCode: 42, reason: "bye" });

			const err = await created;
			expect(err).toBeInstanceOf(Error);
			expect((err as Error).message).toBe("Connection closed: 42 bye");
		});

		test("a bidi stream blocked on stream-count credit rejects with a CONNECTION_CLOSE frame's reason", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsBidi: 0n }) });
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"));

			const created = settleTo(session.createBidirectionalStream());
			await flushMicrotasks();

			// The peer aborts the connection (CONNECTION_CLOSE / 0x1c); the frame's
			// code/reason must reach the waiter.
			peer.send({ type: "connection_close", code: VarInt.from(7n), reason: "peer gone" });

			const err = await created;
			expect(err).toBeInstanceOf(Error);
			expect((err as Error).message).toBe("Connection closed: 7 peer gone");
		});

		test("a stream write blocked on send credit rejects with the close reason", async () => {
			// Tiny per-stream send window: the first chunk drains it, the next write parks.
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamDataUni: 5n }) });

			const writable = await session.createUnidirectionalStream();
			const writer = writable.getWriter();
			const wrote = settleTo(writer.write(new Uint8Array(8)));

			// The first 5 bytes flush to the wire, so the write is now parked on the remaining 3.
			await waitFor(() => peer.has("stream"));

			session.close({ closeCode: 9, reason: "credit gone" });

			const err = await wrote;
			expect(err).toBeInstanceOf(Error);
			expect((err as Error).message).toBe("Connection closed: 9 credit gone");
		});

		test("a blocked claim rejects with the reason from a WebSocket-level close", async () => {
			const { session, peer } = connect();
			await session.ready;
			peer.send({ type: "transport_parameters", params: peerParams({ initialMaxStreamsUni: 0n }) });
			peer.send({ type: "ping_request", sequence: 1n });
			await waitFor(() => peer.has("ping_response"));

			const created = settleTo(session.createUnidirectionalStream());
			await flushMicrotasks();

			// The transport closes underneath the session (no CONNECTION_CLOSE frame).
			peer.close({ closeCode: 1001, reason: "going away" });

			const err = await created;
			expect(err).toBeInstanceOf(Error);
			expect((err as Error).message).toBe("Connection closed: 1001 going away");
		});
	});

	test("createBidirectionalStream on an already-closed session rejects with the descriptive reason", async () => {
		const { session } = connect();
		await session.ready;

		// Graceful app-initiated close leaves #closeReason unset, so the synchronous
		// already-closed throw must fall back to #closed's "Connection closed: <code> <reason>",
		// not a generic "Connection closed".
		session.close({ closeCode: 5, reason: "done" });

		const err = await settleTo(session.createBidirectionalStream());
		expect(err).toBeInstanceOf(Error);
		expect((err as Error).message).toBe("Connection closed: 5 done");
	});
});

/** Hand `Session.accept` a scripted socket, standing in for one a host handed us
 *  after an HTTP upgrade. The subprotocol is already negotiated at this point, so
 *  the peer reports it rather than the session advertising it. */
function accept(options?: { config?: Config; protocol?: string }): { session: Session; peer: FakePeer } {
	const peer = new FakePeer("wss://example/test", { protocol: options?.protocol });
	const session = Session.accept(peer as unknown as WebSocketStreamLike, {
		// Idle timer off so a stray interval can't interfere with the test.
		config: { idleTimeout: null, ...options?.config },
	});
	return { session, peer };
}

/** The first STREAM frame the session put on the wire. */
async function firstStreamId(peer: FakePeer): Promise<Stream.Id> {
	await waitFor(() => peer.has("stream"));
	return (peer.received().find((f) => f.type === "stream") as Frame.Data).id;
}

describe("Session.accept (server role)", () => {
	for (const protocol of ["", "webtransport", "unknown", "qmux-99"]) {
		test(`refuses unsupported subprotocol ${JSON.stringify(protocol)}`, async () => {
			const { session, peer } = accept({ protocol });
			expect(await settleTo(session.ready)).toBeInstanceOf(Error);
			const error = await expectSessionFailure(session);
			expect((error.cause as Error).message).toContain("Unsupported QMux subprotocol");
			expect(peer.sent).toEqual([]);
			expect(peer.closeInfo).toBeDefined();
		});
	}

	test("streams we open are server-initiated", async () => {
		// The whole point of the role: a server minting client-initiated ids would
		// collide with the client's own streams.
		const { session, peer } = accept();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const writable = await session.createUnidirectionalStream();
		await writable.getWriter().write(new Uint8Array([1]));

		const id = await firstStreamId(peer);
		expect(id.serverInitiated).toBe(true);
		expect(id.dir).toBe(Stream.Dir.Uni);
		session.close();
	});

	test("bidi streams we open are server-initiated", async () => {
		const { session, peer } = accept();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const stream = await session.createBidirectionalStream();
		await stream.writable.getWriter().write(new Uint8Array([1]));

		const id = await firstStreamId(peer);
		expect(id.serverInitiated).toBe(true);
		expect(id.dir).toBe(Stream.Dir.Bi);
		session.close();
	});

	test("a client-initiated stream arrives as an incoming stream", async () => {
		const { session, peer } = accept();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// Client-initiated (isServer: false) — the mirror of what a client session
		// accepts, and what a client session would reject as a violation.
		peer.send({
			type: "stream",
			id: Stream.Id.create(0n, Stream.Dir.Uni, false),
			data: new Uint8Array([7, 8, 9]),
			fin: true,
		});

		const reader = session.incomingUnidirectionalStreams.getReader();
		const { value: incoming } = await reader.read();
		const { value } = await (incoming as ReadableStream<Uint8Array>).getReader().read();
		expect(value).toEqual(new Uint8Array([7, 8, 9]));
		reader.releaseLock();
		session.close();
	});

	test("a peer using our own role's stream ids is a protocol violation", async () => {
		const { session, peer } = accept();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		// Only the server may open server-initiated uni streams; a client sending
		// one is either confused or spoofing our side of the id space.
		peer.send({
			type: "stream",
			id: Stream.Id.create(0n, Stream.Dir.Uni, true),
			data: new Uint8Array([1]),
			fin: false,
		});

		await waitFor(() => peer.has("connection_close"));
		expect(sentCloseCode(peer)).toBe(1002);
		await expectSessionFailure(session);
	});

	test("ArrayBuffer chunks decode on a transport we were handed directly", async () => {
		// `openWebSocketStream` normalizes the native API's ArrayBuffer chunks, but
		// accept() gets the transport as-is — and Chromium's WebSocketStream yields
		// ArrayBuffer. Without normalization in the read loop this throws
		// "Expected ArrayBuffer for the first argument" out of the varint decoder
		// and the session never opens a stream.
		const { session, peer } = accept();
		await session.ready;
		peer.sendArrayBuffer({ type: "transport_parameters", params: peerParams() });

		// Decoding the params is what grants the stream-count credit this claims.
		const writable = await session.createUnidirectionalStream();
		await writable.getWriter().write(new Uint8Array([1]));
		expect((await firstStreamId(peer)).serverInitiated).toBe(true);
		session.close();
	});

	test("the wire format comes from the socket's negotiated subprotocol", async () => {
		const { session, peer } = accept({ protocol: "qmux-01.moq-lite-04" });
		await session.ready;
		expect(session.protocol).toBe("moq-lite-04");
		// A QMux draft was negotiated, so we owe the peer our parameters.
		await waitFor(() => peer.has("transport_parameters"));
		session.close();
	});

	test("the protocol option stands in for a socket that doesn't report one", async () => {
		// Without the override an empty negotiated subprotocol is refused.
		const peer = new FakePeer("wss://example/test", { protocol: "" });
		const session = Session.accept(peer as unknown as WebSocketStreamLike, {
			config: { idleTimeout: null },
			protocol: "qmux-01.moq-lite-04",
		});
		await session.ready;
		expect(session.protocol).toBe("moq-lite-04");
		await waitFor(() => peer.has("transport_parameters"));
		session.close();
	});

	test("a plain WebSocket is adopted, not just a WebSocketStream", async () => {
		// What Deno.upgradeWebSocket / `ws` actually hand back.
		const ws = new FakeServerSocket("wss://example/test", "qmux-01");
		const session = Session.accept(ws, { config: { idleTimeout: null } });
		await session.ready;

		await waitFor(() => ws.sent.length > 0);
		const [params] = Frame.decodeRecord(ws.sent[0] as Uint8Array);
		expect(params.type).toBe("transport_parameters");
		session.close();
	});
});

// STREAM frame sizing. What we accept is bounded by the `max_record_size` we
// advertised (enforced on the record, not on the frame inside it). What we send
// also keeps the compatibility ceiling required by released receivers.
describe("STREAM frame size", () => {
	afterEach(() => {
		if (ORIGINAL_WSS === undefined) {
			delete (globalThis as { WebSocketStream?: unknown }).WebSocketStream;
		} else {
			(globalThis as { WebSocketStream?: unknown }).WebSocketStream = ORIGINAL_WSS;
		}
	});

	/** Read one incoming uni stream to completion. */
	async function readIncomingUni(session: Session): Promise<Uint8Array> {
		const incoming = await session.incomingUnidirectionalStreams.getReader().read();
		if (!incoming.value) throw new Error("expected an incoming stream");
		const reader = incoming.value.getReader();
		const chunks: Uint8Array[] = [];
		while (true) {
			const { value, done } = await reader.read();
			if (done) break;
			chunks.push(value);
		}
		const out = new Uint8Array(chunks.reduce((n, c) => n + c.byteLength, 0));
		let offset = 0;
		for (const chunk of chunks) {
			out.set(chunk, offset);
			offset += chunk.byteLength;
		}
		return out;
	}

	test("a record-sized STREAM frame is delivered", async () => {
		// A 5-byte STREAM header plus 16377 payload bytes is a record of exactly the
		// max_record_size we advertised: refusing it refuses the very size the
		// handshake invited. A peer chunking at any other header budget — an older
		// release, a third-party stack — lands somewhere in this range too.
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, true);
		const data = new Uint8Array(Number(DEFAULT_MAX_RECORD_SIZE) - 5).fill(0x5a);
		const frame: Frame.Data = { type: "stream", id, offset: 0n, data, fin: true };
		expect(Frame.encode(frame, "qmux-01").byteLength).toBe(Number(DEFAULT_MAX_RECORD_SIZE));
		peer.send(frame);

		expect(await readIncomingUni(session)).toEqual(data);
		session.close();
	});

	test("qmux-00 delivers a frame of exactly max_frame_size", async () => {
		// draft-00 §5.2 bounds the whole frame, header included, so a 16384-byte
		// frame is legal — and the parameter may only ever be raised.
		const { session, peer } = accept({ protocol: "qmux-00" });
		await session.ready;

		const id = Stream.Id.create(0n, Stream.Dir.Uni, false);
		const data = new Uint8Array(Frame.MAX_FRAME_SIZE - 5).fill(0x5a);
		const frame = Frame.encode({ type: "stream", id, offset: 0n, data, fin: true }, "qmux-00");
		expect(frame.byteLength).toBe(Frame.MAX_FRAME_SIZE);
		peer.sendRaw(frame);

		expect(await readIncomingUni(session)).toEqual(data);
		session.close();
	});

	test("a qmux-00 frame over max_frame_size closes the session", async () => {
		// One byte past the limit is a frame-size violation, which draft-00 §5.2
		// requires closing the connection over.
		const { session, peer } = accept({ protocol: "qmux-00" });
		await session.ready;

		const id = Stream.Id.create(0n, Stream.Dir.Uni, false);
		const data = new Uint8Array(Frame.MAX_FRAME_SIZE - 4).fill(0x5a);
		const frame = Frame.encode({ type: "stream", id, offset: 0n, data, fin: true }, "qmux-00");
		expect(frame.byteLength).toBe(Frame.MAX_FRAME_SIZE + 1);
		peer.sendRaw(frame);

		const err = await expectSessionFailure(session);
		expect((err.cause as Error).message).toContain("max_frame_size");
		await waitFor(() => sentCloseCode(peer) !== undefined);
		expect(sentCloseCode(peer)).toBe(1002);
	});

	test("a record over max_record_size closes the session", async () => {
		// The record is the limit a peer can actually derive from the handshake, so
		// it is the one worth enforcing — and a record too large to parse is a
		// connection-level violation.
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		peer.sendRaw(new Uint8Array(Number(DEFAULT_MAX_RECORD_SIZE) + 1));

		const err = await expectSessionFailure(session);
		expect((err.cause as Error).message).toContain("max_record_size");
		await waitFor(() => sentCloseCode(peer) !== undefined);
		expect(sentCloseCode(peer)).toBe(1002);
	});

	test("we keep STREAM payloads within the released-receiver ceiling", async () => {
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const writable = await session.createUnidirectionalStream();
		await writable.getWriter().write(new Uint8Array(Number(DEFAULT_MAX_RECORD_SIZE) * 2).fill(0x5a));

		await waitFor(() => peer.has("stream"));
		const stream = peer.received().find((f) => f.type === "stream") as Frame.Data;
		expect(stream.data.byteLength).toBe(Frame.MAX_FRAME_PAYLOAD);
		session.close();
	});

	test("a peer advertising the largest max_record_size can still be written to", async () => {
		// max_record_size is a varint, so 2^62-1 is legal — and is not a value
		// `number` holds exactly, so the budget has to stay a bigint.
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams({ maxRecordSize: VarInt.MAX }) });

		const writable = await session.createUnidirectionalStream();
		const data = new Uint8Array(1024).fill(0x5a);
		await writable.getWriter().write(data);

		await waitFor(() => peer.has("stream"));
		const stream = peer.received().find((f) => f.type === "stream") as Frame.Data;
		expect(stream.data).toEqual(data);
		session.close();
	});

	test("a larger peer record limit does not bypass the compatibility ceiling", async () => {
		const maxRecordSize = 32_768n;
		const { session, peer } = connect();
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams({ maxRecordSize }) });

		const writable = await session.createUnidirectionalStream();
		await writable.getWriter().write(new Uint8Array(Number(maxRecordSize) * 2).fill(0x5a));

		await waitFor(() => peer.has("stream"));
		const stream = peer.received().find((f) => f.type === "stream") as Frame.Data;
		expect(stream.data.byteLength).toBe(Frame.MAX_FRAME_PAYLOAD);
		session.close();
	});
});

/** Encode a RESET_STREAM_AT (0x24) frame, which our own encoder never emits. */
function resetStreamAt(id: Stream.Id, code: number, finalSize: bigint, reliableSize: bigint): Uint8Array {
	let buffer = new Uint8Array(new ArrayBuffer(1 + 8 * 4), 0, 1);
	buffer[0] = 0x24;
	buffer = id.value.encode(buffer);
	buffer = VarInt.from(code).encode(buffer);
	buffer = VarInt.from(finalSize).encode(buffer);
	buffer = VarInt.from(reliableSize).encode(buffer);
	return buffer;
}

describe("RESET_STREAM_AT (qmux-02)", () => {
	/** A qmux-02 server session with one client-initiated uni stream carrying `chunks`. */
	async function open(chunks: number[][], config?: Config) {
		const { session, peer } = accept({ protocol: "qmux-02", config });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, false);
		let offset = 0n;
		for (const chunk of chunks) {
			peer.send({ type: "stream", id, offset, data: new Uint8Array(chunk), fin: false });
			offset += BigInt(chunk.length);
		}

		const acceptor = session.incomingUnidirectionalStreams.getReader();
		const incoming = await acceptor.read();
		acceptor.releaseLock();
		if (incoming.done || !incoming.value) throw new Error("expected an incoming stream");
		return { session, peer, id, reader: incoming.value.getReader() };
	}

	async function readError(reader: ReadableStreamDefaultReader<Uint8Array>): Promise<unknown> {
		return reader.read().then(
			() => undefined,
			(e: unknown) => e,
		);
	}

	/** Send a RESET_STREAM_AT and wait until the session has processed it, so a
	 *  read can't race ahead and pull the buffered bytes first. */
	async function reset(peer: FakePeer, frame: Uint8Array) {
		peer.sendRaw(frame);
		peer.send({ type: "ping_request", sequence: 1n });
		await waitFor(() => peer.has("ping_response"));
	}

	test("the reliable prefix is delivered before the reset surfaces", async () => {
		const { session, peer, id, reader } = await open([
			[1, 2, 3],
			[4, 5, 6],
		]);
		await reset(peer, resetStreamAt(id, 7, 6n, 4n));

		expect(Array.from((await reader.read()).value ?? [])).toEqual([1, 2, 3]);
		expect(Array.from((await reader.read()).value ?? [])).toEqual([4]);
		const err = await readError(reader);
		expect(err).toBeInstanceOf(StreamError);
		expect((err as StreamError).streamErrorCode).toBe(7);
		session.close();
	});

	test("a prefix the reader already has resets immediately", async () => {
		const { session, peer, id, reader } = await open([[1, 2, 3]]);
		expect(Array.from((await reader.read()).value ?? [])).toEqual([1, 2, 3]);

		await reset(peer, resetStreamAt(id, 7, 3n, 2n));
		expect(await readError(reader)).toBeInstanceOf(StreamError);
		session.close();
	});

	test("the delivered prefix and the discarded tail both return connection credit", async () => {
		// A 10-byte window replenishes once 5 or more are consumed: the discarded
		// tail (4) or the prefix (6) alone stays below that, so MAX_DATA 20 needs both.
		const { session, peer, id, reader } = await open([[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]], { receiveWindow: 10n });
		await reset(peer, resetStreamAt(id, 0, 10n, 6n));
		expect(peer.has("max_data")).toBe(false);

		expect(Array.from((await reader.read()).value ?? [])).toEqual([1, 2, 3, 4, 5, 6]);
		expect(await readError(reader)).toBeInstanceOf(StreamError);

		await waitFor(() => peer.has("max_data"));
		expect((peer.received().find((f) => f.type === "max_data") as Frame.MaxData).max).toBe(20n);
		session.close();
	});

	test("a refused stream's reliable prefix still returns connection credit", async () => {
		const { session, peer } = accept({ protocol: "qmux-02", config: { receiveWindow: 10n } });
		await session.ready;
		peer.send({ type: "transport_parameters", params: peerParams() });

		const id = Stream.Id.create(0n, Stream.Dir.Uni, false);
		peer.send({ type: "stream", id, offset: 0n, data: new Uint8Array(10), fin: false });
		await reset(peer, resetStreamAt(id, 0, 10n, 6n));
		expect(peer.has("max_data")).toBe(false);

		// Refusing the unaccepted stream frees the kept prefix too.
		await session.incomingUnidirectionalStreams.cancel();
		await waitFor(() => peer.has("max_data"));
		expect((peer.received().find((f) => f.type === "max_data") as Frame.MaxData).max).toBe(20n);
		session.close();
	});

	test("a reliable prefix beyond the bytes received is a protocol violation", async () => {
		const { session, peer, id } = await open([[1, 2]]);
		// The transport is ordered, so bytes 2..4 can never arrive after the reset.
		peer.sendRaw(resetStreamAt(id, 0, 4n, 4n));

		const err = await expectSessionFailure(session);
		expect((err.cause as Error).message).toContain("reliable_size");
		await waitFor(() => sentCloseCode(peer) === 1002);
	});
});

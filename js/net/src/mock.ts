/**
 * Test doubles for in-process testing.
 *
 * Creates paired client/server transports connected by in-memory streams.
 */

import { Consumer as BroadcastConsumer } from "./broadcast.ts";
import type { TransportStats } from "./connection/stats.ts";
import type { Frame } from "./group.ts";
import type { SendStream } from "./stream.ts";
import { Timestamp } from "./time.ts";

/** A UTF-8 text frame stamped `timestamp` (now by default), for a test writing to a timed track. */
export function textFrame(text: string, timestamp = Timestamp.now()): Frame {
	return { payload: new TextEncoder().encode(text), timestamp };
}

/**
 * A broadcast as a session hands it out: no producer, so it ends once no handle holds it,
 * which is how a test sees the origin let go of an answer.
 */
export class SessionBroadcast extends BroadcastConsumer {
	// biome-ignore lint/complexity/noUselessConstructor: widens the protected base constructor to public
	constructor() {
		super();
	}
}

// High watermark to prevent writes from blocking on backpressure.
// Real WebTransport has kernel buffers; we simulate this with a large queue.
const WRITABLE_STRATEGY: QueuingStrategy<Uint8Array> = { highWaterMark: 256 };
const READABLE_STRATEGY: QueuingStrategy<Uint8Array> = { highWaterMark: 256 };

/** Every stream of a transport pair, so a session close can fail the ones still open. */
type StreamSet = Set<(err: Error) => void>;

/**
 * What the peer sees of a local reset or stop reason: only its code, as on a real wire.
 * WebTransport sends a `WebTransportError`'s `streamErrorCode` and 0 for anything else.
 */
function onWire(reason: unknown, message: string): Error {
	const { source, streamErrorCode } = (reason ?? {}) as { source?: unknown; streamErrorCode?: unknown };
	const code = source === "stream" && typeof streamErrorCode === "number" ? streamErrorCode : 0;
	return Object.assign(new Error(`${message}: ${code}`), { source: "stream" as const, streamErrorCode: code });
}

/**
 * One direction of a stream. A reset (`abort`) reaches the reader, and a stop (`cancel`) the
 * writer, carrying only the code. A TransformStream would hand the peer the local reason.
 */
function newStream(streams: StreamSet): { readable: ReadableStream<Uint8Array>; writable: WritableStream<Uint8Array> } {
	let reader!: ReadableStreamDefaultController<Uint8Array>;
	let writer!: WritableStreamDefaultController;
	// Set once either side ends early, so a write waiting on a full reader gives up.
	let ended = false;
	let wake: (() => void) | undefined;
	const resume = () => {
		wake?.();
		wake = undefined;
	};
	const end = () => {
		ended = true;
		resume();
	};

	const readable = new ReadableStream<Uint8Array>(
		{
			start(controller) {
				reader = controller;
			},
			pull: resume,
			cancel(reason) {
				writer.error(onWire(reason, "stop sending"));
				end();
			},
		},
		READABLE_STRATEGY,
	);

	const writable = new WritableStream<Uint8Array>(
		{
			start(controller) {
				writer = controller;
				controller.signal.addEventListener("abort", end);
			},
			async write(chunk) {
				// Copy each chunk to simulate real WebTransport's kernel-boundary copy.
				// Without this, Writer's scratch buffer reuse corrupts queued data.
				reader.enqueue(new Uint8Array(chunk));
				// Backpressure: wait for the reader to drain, as a TransformStream would.
				while (!ended && (reader.desiredSize ?? 0) <= 0) {
					await new Promise<void>((resolve) => {
						wake = resolve;
					});
				}
			},
			close() {
				reader.close();
			},
			abort(reason) {
				reader.error(onWire(reason, "reset stream"));
			},
		},
		WRITABLE_STRATEGY,
	);

	streams.add((err) => {
		reader.error(err);
		writer.error(err);
		end();
	});

	return { readable, writable };
}

/** A fake {@link WebTransport} backed by in-memory streams, paired with a peer. */
export class MockTransport implements WebTransport {
	readonly protocol: string;
	readonly ready: Promise<undefined>;
	readonly closed: Promise<WebTransportCloseInfo>;

	readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
	readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;

	readonly datagrams: WebTransportDatagramDuplexStream;
	readonly congestionControl: WebTransportCongestionControl;
	readonly reliability = "supports-unreliable";

	/**
	 * Every outgoing stream this transport opened, in call order, split by stream kind. Each
	 * carries a live {@link SendStream.sendOrder} (set when it opened, and by any later
	 * re-rank), so a test can assert how the wire layer prioritized it. The mock always
	 * delivers in creation order, so nothing else observes it.
	 */
	readonly sendStreams: { uni: SendStream[]; bidi: SendStream[] } = { uni: [], bidi: [] };

	#closeResolve!: (info: WebTransportCloseInfo) => void;
	#bidiController!: ReadableStreamDefaultController<WebTransportBidirectionalStream>;
	#uniController!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;
	// Incoming-datagram controller (only set when datagrams are enabled); the peer's
	// writable enqueues into it.
	#datagramController?: ReadableStreamDefaultController<Uint8Array>;

	// Reference to the peer so we can enqueue streams to them
	#peer?: MockTransport;

	// Shared with the peer: a session close fails every stream either side opened, the way
	// a real WebTransport session does.
	#streams: StreamSet = new Set();

	constructor(
		protocol: string,
		datagramsEnabled = true,
		datagramWritable: DatagramWritableApi = "writable",
		datagramReadable = true,
	) {
		this.protocol = protocol;
		this.ready = Promise.resolve(undefined);
		this.closed = new Promise((resolve) => {
			this.#closeResolve = resolve;
		});

		this.incomingBidirectionalStreams = new ReadableStream({
			start: (controller) => {
				this.#bidiController = controller;
			},
		});

		this.incomingUnidirectionalStreams = new ReadableStream({
			start: (controller) => {
				this.#uniController = controller;
			},
		});

		this.congestionControl = "default";

		if (datagramsEnabled) {
			// A real datagram duplex: writes enqueue into the peer's incoming readable,
			// so a peer pair actually exchanges datagrams (unlike a real qmux Session,
			// whose datagram streams are inert stubs).
			const readable = datagramReadable
				? new ReadableStream<Uint8Array>({
						start: (controller) => {
							this.#datagramController = controller;
						},
					})
				: undefined;
			const writable = new WritableStream<Uint8Array>({
				write: (chunk) => {
					const peer = this.#peer;
					if (!peer) return;
					const controller = peer.#datagramController;
					if (!controller) return;
					try {
						// Copy so the caller's scratch buffer reuse can't corrupt queued data.
						controller.enqueue(new Uint8Array(chunk));
					} catch {
						// Peer closed; drop (datagrams are best-effort).
					}
				},
			});
			const datagrams: {
				readable?: ReadableStream<Uint8Array>;
				writable?: WritableStream<Uint8Array>;
				maxDatagramSize: number;
				incomingHighWaterMark: number;
				outgoingHighWaterMark: number;
				incomingMaxAge: null;
				outgoingMaxAge: null;
				createWritable?: () => WritableStream<Uint8Array>;
			} = {
				readable,
				incomingHighWaterMark: 0,
				outgoingHighWaterMark: 0,
				incomingMaxAge: null,
				outgoingMaxAge: null,
				maxDatagramSize: 1200,
			};
			if (datagramWritable === "writable") {
				datagrams.writable = writable;
			} else if (datagramWritable === "createWritable") {
				datagrams.createWritable = () => writable;
			}
			this.datagrams = datagrams as WebTransportDatagramDuplexStream;
		} else {
			// Simulate a transport that doesn't carry datagrams (maxDatagramSize 0): the
			// wire layer must fall back to not sending, with no group fallback.
			this.datagrams = {
				readable: new ReadableStream(),
				writable: new WritableStream(),
				incomingHighWaterMark: 0,
				outgoingHighWaterMark: 0,
				incomingMaxAge: null,
				outgoingMaxAge: null,
				maxDatagramSize: 0,
			};
		}
	}

	setPeer(peer: MockTransport) {
		this.#peer = peer;
		this.#streams = peer.#streams;
	}

	async createBidirectionalStream(options?: WebTransportSendStreamOptions): Promise<WebTransportBidirectionalStream> {
		const peer = this.#peer;
		if (!peer) throw new Error("no peer");

		// One stream for each direction
		const c2s = newStream(this.#streams);
		const s2c = newStream(this.#streams);

		// Local side: writes to c2s, reads from s2c
		const local = {
			readable: s2c.readable,
			writable: this.#sendStream(c2s.writable, this.sendStreams.bidi, options),
		} as WebTransportBidirectionalStream;

		// Peer side: writes to s2c, reads from c2s
		const remote = {
			readable: c2s.readable,
			writable: s2c.writable,
		} as WebTransportBidirectionalStream;

		try {
			peer.#bidiController.enqueue(remote);
		} catch {
			// Peer closed
		}

		return local;
	}

	async createUnidirectionalStream(options?: WebTransportSendStreamOptions): Promise<WritableStream<Uint8Array>> {
		const peer = this.#peer;
		if (!peer) throw new Error("no peer");

		const c2s = newStream(this.#streams);

		// Record before handing the peer its end, so a peer that waits for the stream to arrive
		// always finds it here.
		const stream = this.#sendStream(c2s.writable, this.sendStreams.uni, options);

		try {
			peer.#uniController.enqueue(c2s.readable);
		} catch {
			// Peer closed
		}

		return stream;
	}

	// Give a writable the send order it opened with and record it, so a test sees both that
	// order and any later re-rank (which assigns the same property, as on a real send stream).
	#sendStream(
		writable: WritableStream<Uint8Array>,
		log: SendStream[],
		options?: WebTransportSendStreamOptions,
	): SendStream {
		const stream = writable as SendStream;
		stream.sendOrder = options?.sendOrder;
		log.push(stream);
		return stream;
	}

	close(_closeInfo?: WebTransportCloseInfo): void {
		const info = _closeInfo ?? { closeCode: 0, reason: "" };
		this.#closeResolve(info);

		// Shaped like the WebTransportError a real session close fails its streams with.
		const closed = Object.assign(new Error("session closed"), { source: "session", streamErrorCode: null });
		for (const fail of this.#streams) {
			fail(closed);
		}
		this.#streams.clear();

		try {
			this.#bidiController.close();
		} catch {
			// Already closed
		}
		try {
			this.#uniController.close();
		} catch {
			// Already closed
		}

		try {
			this.#datagramController?.close();
		} catch {
			// Already closed
		}

		// Also close peer's incoming streams
		if (this.#peer) {
			try {
				this.#peer.#bidiController.close();
			} catch {
				// Already closed
			}
			try {
				this.#peer.#uniController.close();
			} catch {
				// Already closed
			}
			try {
				this.#peer.#datagramController?.close();
			} catch {
				// Already closed
			}
			this.#peer.#closeResolve(info);
		}
	}

	/** The snapshot {@link getStats} resolves with (default empty). */
	stats: TransportStats = {};

	/** `datagrams` is the one member the W3C stats dictionary requires. */
	async getStats() {
		return { datagrams: {}, ...this.stats };
	}
}

type DatagramWritableApi = "writable" | "createWritable" | "none";

/** Options for {@link createMockTransportPair}. */
export interface MockTransportOptions {
	/**
	 * Whether the paired transports carry QUIC datagrams (default true). Set false to
	 * simulate a transport that reports `maxDatagramSize` 0 (e.g. a qmux/WebSocket
	 * session), so the wire layer must fall back to not sending datagrams.
	 */
	datagrams?: boolean;

	/** Which outgoing datagram API shape the mock exposes. */
	datagramWritable?: DatagramWritableApi;

	/** Whether the incoming datagram readable stream is exposed. */
	datagramReadable?: boolean;

	/** The snapshot `getStats()` resolves with on both transports (default empty). */
	stats?: TransportStats;
}

/**
 * Creates a pair of connected MockTransport instances.
 *
 * @param protocol - The WebTransport protocol identifier (e.g. "moqt-18", "moql", "")
 * @param options - Optional behavior toggles for datagram availability
 * @returns An object containing `client` and `server` transports
 */
export function createMockTransportPair(
	protocol = "",
	options?: MockTransportOptions,
): { client: MockTransport; server: MockTransport } {
	const datagrams = options?.datagrams ?? true;
	const client = new MockTransport(protocol, datagrams, options?.datagramWritable, options?.datagramReadable);
	const server = new MockTransport(protocol, datagrams, options?.datagramWritable, options?.datagramReadable);
	if (options?.stats) {
		// Copy per side so a test advancing one transport's counters doesn't move the other's.
		client.stats = { ...options.stats };
		server.stats = { ...options.stats };
	}
	client.setPeer(server);
	server.setPeer(client);
	return { client, server };
}

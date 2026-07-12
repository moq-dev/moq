/**
 * Mock WebTransport implementation for in-process testing.
 *
 * Creates paired client/server transports connected via TransformStreams.
 */

// High watermark to prevent writes from blocking on backpressure.
// Real WebTransport has kernel buffers; we simulate this with a large queue.
const WRITABLE_STRATEGY: QueuingStrategy<Uint8Array> = { highWaterMark: 256 };
const READABLE_STRATEGY: QueuingStrategy<Uint8Array> = { highWaterMark: 256 };

function newStream(): TransformStream<Uint8Array, Uint8Array> {
	return new TransformStream(
		{
			// Copy each chunk to simulate real WebTransport's kernel-boundary copy.
			// Without this, Writer's scratch buffer reuse corrupts queued data.
			transform(chunk, controller) {
				controller.enqueue(new Uint8Array(chunk));
			},
		},
		WRITABLE_STRATEGY,
		READABLE_STRATEGY,
	);
}

class MockTransport implements WebTransport {
	readonly protocol: string;
	readonly ready: Promise<undefined>;
	readonly closed: Promise<WebTransportCloseInfo>;

	readonly incomingBidirectionalStreams: ReadableStream<WebTransportBidirectionalStream>;
	readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;

	readonly datagrams: WebTransportDatagramDuplexStream;
	readonly congestionControl: WebTransportCongestionControl;
	readonly reliability: string;

	#closeResolve!: (info: WebTransportCloseInfo) => void;
	#bidiController!: ReadableStreamDefaultController<WebTransportBidirectionalStream>;
	#uniController!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;
	// Incoming-datagram controller (only set when datagrams are enabled); the peer's
	// writable enqueues into it.
	#datagramController?: ReadableStreamDefaultController<Uint8Array>;

	// Reference to the peer so we can enqueue streams to them
	#peer?: MockTransport;

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
		this.reliability = "supports-unreliable";

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
	}

	async createBidirectionalStream(
		_options?: WebTransportSendStreamOptions,
	): Promise<WebTransportBidirectionalStream> {
		const peer = this.#peer;
		if (!peer) throw new Error("no peer");

		// Create two TransformStreams for the two directions
		const c2s = newStream();
		const s2c = newStream();

		// Local side: writes to c2s, reads from s2c
		const local = {
			readable: s2c.readable,
			writable: c2s.writable,
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

	async createUnidirectionalStream(_options?: WebTransportSendStreamOptions): Promise<WritableStream<Uint8Array>> {
		const peer = this.#peer;
		if (!peer) throw new Error("no peer");

		const c2s = newStream();

		try {
			peer.#uniController.enqueue(c2s.readable);
		} catch {
			// Peer closed
		}

		return c2s.writable;
	}

	close(_closeInfo?: WebTransportCloseInfo): void {
		const info = _closeInfo ?? { closeCode: 0, reason: "" };
		this.#closeResolve(info);

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

	// biome-ignore lint/suspicious/noExplicitAny: WebTransportStats type not available in all TS libs
	async getStats(): Promise<any> {
		return {};
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
}

/**
 * Creates a pair of connected MockTransport instances.
 *
 * @param protocol - The WebTransport protocol identifier (e.g. "moqt-17", "moql", "")
 * @param options - Optional behavior toggles for datagram availability
 * @returns An object containing `client` and `server` transports
 */
export function createMockTransportPair(
	protocol = "",
	options?: MockTransportOptions,
): { client: WebTransport; server: WebTransport } {
	const datagrams = options?.datagrams ?? true;
	const client = new MockTransport(protocol, datagrams, options?.datagramWritable, options?.datagramReadable);
	const server = new MockTransport(protocol, datagrams, options?.datagramWritable, options?.datagramReadable);
	client.setPeer(server);
	server.setPeer(client);
	return { client, server };
}

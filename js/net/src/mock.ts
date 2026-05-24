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

	// Reference to the peer so we can enqueue streams to them
	#peer?: MockTransport;

	constructor(protocol: string) {
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

		// Stub datagrams
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
			this.#peer.#closeResolve(info);
		}
	}

	// biome-ignore lint/suspicious/noExplicitAny: WebTransportStats type not available in all TS libs
	async getStats(): Promise<any> {
		return {};
	}
}

/**
 * Creates a pair of connected MockTransport instances.
 *
 * @param protocol - The WebTransport protocol identifier (e.g. "moqt-17", "moql", "")
 * @returns An object containing `client` and `server` transports
 */
export function createMockTransportPair(protocol = ""): { client: WebTransport; server: WebTransport } {
	const client = new MockTransport(protocol);
	const server = new MockTransport(protocol);
	client.setPeer(server);
	server.setPeer(client);
	return { client, server };
}

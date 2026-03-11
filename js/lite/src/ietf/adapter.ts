import { Mutex } from "async-mutex";
import { Reader, Stream, type Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import * as Namespace from "./namespace.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * Interface for opening outgoing bidi streams and allocating request IDs.
 * Implemented by both ControlStreamAdapter (v14-v16) and NativeSession (v17).
 */
export interface Session {
	openBi(requestId: bigint): Stream | Promise<Stream>;
	acceptBi(): Promise<Stream | undefined>;
	nextRequestId(): Promise<bigint | undefined>;
	registerNamespace?(namespace: string, requestId: bigint): void;
	readonly version: IetfVersion;
}

/**
 * v17 native session — thin wrapper around WebTransport.
 * Each request gets its own real bidi stream; no control stream multiplexing.
 */
export class NativeSession implements Session {
	#quic: WebTransport;
	#requestId = 0n;
	readonly version: IetfVersion;

	constructor(quic: WebTransport, version: IetfVersion) {
		this.#quic = quic;
		this.version = version;
	}

	async openBi(_requestId: bigint): Promise<Stream> {
		return Stream.open(this.#quic);
	}

	async acceptBi(): Promise<Stream | undefined> {
		return Stream.accept(this.#quic);
	}

	async nextRequestId(): Promise<bigint | undefined> {
		const id = this.#requestId;
		this.#requestId += 2n;
		return id;
	}
}

// Route classification for control stream messages.
const Route = {
	NewRequest: 0, // Create virtual bidi stream, push initial message
	Response: 1, // Push message to existing stream (keep open)
	ErrorResponse: 2, // Push message to existing stream, then close
	CloseStream: 3, // Close stream recv (no bytes pushed)
	FollowUp: 4, // Push follow-up message to existing stream
	MaxRequestId: 5, // Update flow control
	Ignore: 6, // Connection-level, no routing
	GoAway: 7, // Terminal
} as const;
type Route = (typeof Route)[keyof typeof Route];

interface StreamEntry {
	controller: ReadableStreamDefaultController<Uint8Array>;
}

/**
 * Converts v14-v16 control stream multiplexing into virtual bidi streams.
 *
 * Reads control messages, classifies them, and routes to virtual Stream objects.
 * Each request/response pair gets its own virtual Stream, making all versions
 * look like v17's stream-per-request model.
 */
export class ControlStreamAdapter implements Session {
	// Control stream
	#reader: Reader;
	#writer: Writer;
	#writeMutex = new Mutex();
	readonly version: IetfVersion;

	// Virtual streams keyed by requestId
	#streams = new Map<bigint, StreamEntry>();

	// Namespace → requestId reverse lookup (v14/v15 namespace-keyed messages)
	#namespaces = new Map<string, bigint>();

	// Incoming stream queue (for acceptBi)
	#incomingQueue: Stream[] = [];
	#incomingWaiters: ((stream: Stream | undefined) => void)[] = [];

	// Request ID flow control
	#requestId = 0n;
	#maxRequestId: bigint;
	#maxRequestIdResolves: (() => void)[] = [];

	#closed = false;

	constructor(controlStream: Stream, version: IetfVersion, maxRequestId: bigint) {
		this.#reader = controlStream.reader;
		this.#reader.version = version;
		this.#writer = controlStream.writer;
		this.#writer.version = version;
		this.version = version;
		this.#maxRequestId = maxRequestId;
	}

	/**
	 * Accept the next incoming virtual bidi stream.
	 * Blocks until a new request arrives on the control stream.
	 */
	async acceptBi(): Promise<Stream | undefined> {
		if (this.#closed) return undefined;

		const queued = this.#incomingQueue.shift();
		if (queued) return queued;

		return new Promise<Stream | undefined>((resolve) => {
			this.#incomingWaiters.push(resolve);
		});
	}

	/**
	 * Open an outgoing virtual bidi stream for the given requestId.
	 * The caller must write the request message to the returned stream.
	 * Writes are forwarded to the control stream; responses are routed back.
	 */
	openBi(requestId: bigint): Stream {
		let controller!: ReadableStreamDefaultController<Uint8Array>;
		const readable = new ReadableStream<Uint8Array>({
			start(c) {
				controller = c;
			},
			cancel: () => {
				this.#streams.delete(requestId);
			},
		});

		const sendWritable = this.#createSendWritable();

		const stream = new Stream({ readable, writable: sendWritable });
		stream.reader.version = this.version;
		stream.writer.version = this.version;

		this.#streams.set(requestId, { controller });
		return stream;
	}

	/**
	 * Register a namespace → requestId mapping for outbound publishes.
	 * Needed so inbound namespace-keyed messages (Cancel/Done) in v14/v15 can be routed.
	 */
	registerNamespace(namespace: string, requestId: bigint) {
		this.#namespaces.set(namespace, requestId);
	}

	/**
	 * Allocate the next request ID, blocking if flow control limit reached.
	 */
	async nextRequestId(): Promise<bigint | undefined> {
		for (;;) {
			if (this.#closed) return undefined;
			const id = this.#requestId;
			if (id < this.#maxRequestId) {
				this.#requestId += 2n;
				return id;
			}
			await new Promise<void>((resolve) => {
				this.#maxRequestIdResolves.push(resolve);
			});
		}
	}

	/**
	 * Main run loop — reads control stream messages and routes to virtual streams.
	 * Must be called after construction. Runs until the control stream closes.
	 */
	async run(): Promise<void> {
		try {
			for (;;) {
				const done = await this.#reader.done();
				if (done) break;

				const typeId = await this.#reader.u53();
				const size = await this.#reader.u16();
				const body = await this.#reader.read(size);

				const classified = await this.#classify(typeId, body);

				if (classified.route === Route.GoAway) {
					console.warn("received GOAWAY on control stream");
					return;
				}

				const { route, requestId } = classified;

				switch (route) {
					case Route.NewRequest:
						this.#newRequest(typeId, size, body, requestId);
						break;
					case Route.Response:
						this.#pushMessage(requestId, typeId, size, body);
						break;
					case Route.ErrorResponse:
						this.#pushMessage(requestId, typeId, size, body);
						this.#closeStream(requestId);
						break;
					case Route.CloseStream:
						this.#closeStream(requestId);
						break;
					case Route.FollowUp:
						this.#pushMessage(requestId, typeId, size, body);
						break;
					case Route.MaxRequestId:
						this.#maxRequestId = requestId;
						for (const resolve of this.#maxRequestIdResolves) resolve();
						this.#maxRequestIdResolves = [];
						break;
				}
			}
		} finally {
			this.close();
		}
	}

	#newRequest(typeId: number, size: number, body: Uint8Array, requestId: bigint) {
		let controller!: ReadableStreamDefaultController<Uint8Array>;
		const readable = new ReadableStream<Uint8Array>({
			start(c) {
				controller = c;
			},
			cancel: () => {
				this.#streams.delete(requestId);
			},
		});

		const sendWritable = this.#createSendWritable();

		const stream = new Stream({ readable, writable: sendWritable });
		stream.reader.version = this.version;
		stream.writer.version = this.version;

		this.#streams.set(requestId, { controller });

		// Push initial message bytes so the dispatcher can read typeId + decode
		controller.enqueue(this.#encodeRaw(typeId, size, body));

		// Queue for acceptBi
		const waiter = this.#incomingWaiters.shift();
		if (waiter) {
			waiter(stream);
		} else {
			this.#incomingQueue.push(stream);
		}
	}

	#pushMessage(requestId: bigint, typeId: number, size: number, body: Uint8Array) {
		const entry = this.#streams.get(requestId);
		if (!entry) {
			console.warn(`adapter: no stream for requestId=${requestId} typeId=0x${typeId.toString(16)}`);
			return;
		}
		try {
			entry.controller.enqueue(this.#encodeRaw(typeId, size, body));
		} catch {
			// Stream already closed
		}
	}

	#closeStream(requestId: bigint) {
		const entry = this.#streams.get(requestId);
		if (!entry) return;
		this.#streams.delete(requestId);
		try {
			entry.controller.close();
		} catch {
			// Already closed
		}
	}

	/** Create a WritableStream that forwards writes to the control stream under mutex. */
	#createSendWritable(): WritableStream<Uint8Array> {
		return new WritableStream<Uint8Array>({
			write: (chunk) => this.#writeMutex.runExclusive(() => this.#writer.write(chunk)),
		});
	}

	/** Encode raw message bytes: [typeId varint][size u16 BE][body] */
	#encodeRaw(typeId: number, size: number, body: Uint8Array): Uint8Array {
		// v14-v16 always use QUIC varint
		const typeIdBytes = Varint.encodeTo(new ArrayBuffer(9), typeId);
		const result = new Uint8Array(typeIdBytes.byteLength + 2 + body.byteLength);
		result.set(typeIdBytes, 0);
		const sizeView = new DataView(result.buffer, typeIdBytes.byteLength, 2);
		sizeView.setUint16(0, size);
		result.set(body, typeIdBytes.byteLength + 2);
		return result;
	}

	/**
	 * Classify a control message and extract its requestId for routing.
	 */
	async #classify(
		typeId: number,
		body: Uint8Array,
	): Promise<{ route: typeof Route.GoAway } | { route: Exclude<Route, typeof Route.GoAway>; requestId: bigint }> {
		const readRequestId = async (): Promise<bigint> => {
			const r = new Reader(undefined, body, this.version);
			return await r.u62();
		};

		const readNamespaceRequestId = async (): Promise<bigint> => {
			const r = new Reader(undefined, body, this.version);
			const namespace = await Namespace.decode(r);
			const requestId = this.#namespaces.get(namespace);
			if (requestId === undefined) throw new Error(`unknown namespace: ${namespace}`);
			this.#namespaces.delete(namespace);
			return requestId;
		};

		switch (typeId) {
			// === NewRequest: create virtual stream ===
			case 0x03: // Subscribe
			case 0x16: // Fetch
			case 0x1d: // Publish
			case 0x0d: {
				// TrackStatusRequest
				const requestId = await readRequestId();
				return { route: Route.NewRequest, requestId };
			}
			case 0x06: {
				// PublishNamespace — also store namespace for v14/v15 reverse lookup
				const r = new Reader(undefined, body, this.version);
				const requestId = await r.u62();
				const namespace = await Namespace.decode(r);
				this.#namespaces.set(namespace, requestId);
				return { route: Route.NewRequest, requestId };
			}
			case 0x11: {
				// SubscribeNamespace (v14/v15 only on control stream)
				if (this.version !== Version.DRAFT_14 && this.version !== Version.DRAFT_15) {
					throw new Error("unexpected SubscribeNamespace on control stream");
				}
				const requestId = await readRequestId();
				return { route: Route.NewRequest, requestId };
			}

			// === Response: push bytes, keep stream open ===
			case 0x04: {
				// SubscribeOk
				const requestId = await readRequestId();
				return { route: Route.Response, requestId };
			}
			case 0x18: {
				// FetchOk
				const requestId = await readRequestId();
				return { route: Route.Response, requestId };
			}
			case 0x1e: {
				// PublishOk
				const requestId = await readRequestId();
				return { route: Route.Response, requestId };
			}
			case 0x07: {
				// v14: PublishNamespaceOk, v15+: RequestOk
				const requestId = await readRequestId();
				return { route: Route.Response, requestId };
			}
			case 0x12: {
				// SubscribeNamespaceOk (v14 only)
				if (this.version !== Version.DRAFT_14) throw new Error("unexpected SubscribeNamespaceOk");
				const requestId = await readRequestId();
				return { route: Route.Response, requestId };
			}

			// === ErrorResponse: push bytes + close ===
			case 0x05: {
				// SubscribeError (v14) / RequestError (v15+)
				const requestId = await readRequestId();
				return { route: Route.ErrorResponse, requestId };
			}
			case 0x19: {
				// FetchError (v14 only)
				if (this.version !== Version.DRAFT_14) throw new Error("unexpected FetchError");
				const requestId = await readRequestId();
				return { route: Route.ErrorResponse, requestId };
			}
			case 0x1f: {
				// PublishError (v14 only)
				if (this.version !== Version.DRAFT_14) throw new Error("unexpected PublishError");
				const requestId = await readRequestId();
				return { route: Route.ErrorResponse, requestId };
			}
			case 0x08: {
				// PublishNamespaceError (v14 only); in v16 this is SubscribeNamespaceEntry (bidi only)
				if (this.version !== Version.DRAFT_14) throw new Error("unexpected message 0x08 on control stream");
				const requestId = await readRequestId();
				return { route: Route.ErrorResponse, requestId };
			}
			case 0x13: {
				// SubscribeNamespaceError (v14 only)
				if (this.version !== Version.DRAFT_14) throw new Error("unexpected SubscribeNamespaceError");
				const requestId = await readRequestId();
				return { route: Route.ErrorResponse, requestId };
			}

			// === CloseStream: close recv (no bytes pushed) ===
			case 0x0a: {
				// Unsubscribe
				const requestId = await readRequestId();
				return { route: Route.CloseStream, requestId };
			}
			case 0x0b: {
				// PublishDone
				const requestId = await readRequestId();
				return { route: Route.CloseStream, requestId };
			}
			case 0x17: {
				// FetchCancel
				const requestId = await readRequestId();
				return { route: Route.CloseStream, requestId };
			}
			case 0x09: {
				// PublishNamespaceDone: v16 uses requestId, v14/v15 uses namespace
				if (this.version === Version.DRAFT_16) {
					const requestId = await readRequestId();
					return { route: Route.CloseStream, requestId };
				}
				const requestId = await readNamespaceRequestId();
				return { route: Route.CloseStream, requestId };
			}
			case 0x0c: {
				// PublishNamespaceCancel: v16 uses requestId, v14/v15 uses namespace
				if (this.version === Version.DRAFT_16) {
					const requestId = await readRequestId();
					return { route: Route.CloseStream, requestId };
				}
				const requestId = await readNamespaceRequestId();
				return { route: Route.CloseStream, requestId };
			}
			case 0x14: {
				// UnsubscribeNamespace (v14/v15 only)
				if (this.version !== Version.DRAFT_14 && this.version !== Version.DRAFT_15) {
					throw new Error("unexpected UnsubscribeNamespace");
				}
				const requestId = await readRequestId();
				return { route: Route.CloseStream, requestId };
			}

			// === Utility ===
			case 0x15: {
				// MaxRequestId
				const requestId = await readRequestId();
				return { route: Route.MaxRequestId, requestId };
			}
			case 0x1a: {
				// RequestsBlocked — connection-level, consume and ignore
				await readRequestId();
				return { route: Route.Ignore, requestId: 0n };
			}

			// === Terminal ===
			case 0x10: // GoAway
				return { route: Route.GoAway };

			default:
				throw new Error(`unknown control message type: 0x${typeId.toString(16)}`);
		}
	}

	close() {
		if (this.#closed) return;
		this.#closed = true;

		// Close all virtual streams
		for (const entry of this.#streams.values()) {
			try {
				entry.controller.close();
			} catch {
				// Already closed
			}
		}
		this.#streams.clear();

		// Resolve any waiting acceptBi callers
		for (const waiter of this.#incomingWaiters) {
			waiter(undefined);
		}
		this.#incomingWaiters = [];

		// Clear namespace mappings
		this.#namespaces.clear();

		// Unblock maxRequestId waiters
		for (const resolve of this.#maxRequestIdResolves) resolve();
		this.#maxRequestIdResolves = [];
	}
}

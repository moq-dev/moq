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
	openBi(): Stream | Promise<Stream>;
	openNativeBi?(): Promise<Stream>;
	acceptBi(): Promise<Stream | undefined>;
	nextRequestId(): Promise<bigint | undefined>;
	close?(): void;
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

	async openBi(): Promise<Stream> {
		return Stream.open(this.#quic, this.version);
	}

	async acceptBi(): Promise<Stream | undefined> {
		return Stream.accept(this.#quic, this.version);
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
	// WebTransport session (for opening real bidi streams in v16)
	#quic: WebTransport;

	// Control stream
	#reader: Reader;
	#writer: Writer;
	#writeMutex = new Mutex();
	readonly version: IetfVersion;

	// Virtual streams keyed by requestId
	#streams = new Map<bigint, StreamEntry>();

	// Namespace → requestId reverse lookup (v14/v15 namespace-keyed messages)
	#namespaces = new Map<string, bigint>();

	// requestId → namespace reverse lookup (for cleanup in #closeStream)
	#namespacesByRequestId = new Map<bigint, string>();

	// SubscribeNamespace requestIds — for routing 0x08/0x0E entries that lack requestId (v14/v15)
	#subscribeNamespaces = new Set<bigint>();

	// Incoming stream queue (for acceptBi)
	#incomingQueue: Stream[] = [];
	#incomingWaiters: ((stream: Stream | undefined) => void)[] = [];

	// Request ID flow control
	#requestId = 0n;
	#maxRequestId: bigint;
	#maxRequestIdResolves: (() => void)[] = [];

	#closed = false;

	constructor(quic: WebTransport, controlStream: Stream, version: IetfVersion, maxRequestId: bigint) {
		this.#quic = quic;
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
	 * Open an outgoing virtual bidi stream.
	 * Buffers writes until the first full message is available, parses the
	 * requestId (and namespace for PublishNamespace), self-registers, then
	 * flushes. Subsequent writes go directly to the control stream.
	 */
	openBi(): Stream {
		let controller!: ReadableStreamDefaultController<Uint8Array>;
		let registeredRequestId: bigint | undefined;

		const readable = new ReadableStream<Uint8Array>({
			start(c) {
				controller = c;
			},
			cancel: () => {
				if (registeredRequestId !== undefined) {
					this.#streams.delete(registeredRequestId);
				}
			},
		});

		let buffer = new Uint8Array(0);
		let registered = false;

		const sendWritable = new WritableStream<Uint8Array>({
			write: async (chunk) => {
				// Always accumulate bytes and flush only complete messages
				// to prevent interleaving of partial messages from concurrent virtual streams.
				const newBuf = new Uint8Array(buffer.length + chunk.length);
				newBuf.set(buffer);
				newBuf.set(chunk, buffer.length);
				buffer = newBuf;

				// Try to flush complete messages from the buffer
				for (;;) {
					const boundary = this.#messageSize(buffer);
					if (boundary === undefined) break;

					const toFlush = buffer.subarray(0, boundary);
					buffer = buffer.subarray(boundary);

					if (!registered) {
						// First message: extract requestId and register before flushing
						const parsed = this.#tryParseOutgoing(toFlush);
						if (parsed) {
							registeredRequestId = parsed.requestId;
							this.#streams.set(parsed.requestId, { controller });
							registered = true;
						}
					}

					await this.#writeMutex.runExclusive(() => this.#writer.write(toFlush));
				}
			},
		});

		const stream = new Stream({ readable, writable: sendWritable });
		stream.reader.version = this.version;
		stream.writer.version = this.version;
		return stream;
	}

	/**
	 * Open a real WebTransport bidi stream (for v16 SubscribeNamespace).
	 */
	async openNativeBi(): Promise<Stream> {
		return Stream.open(this.#quic, this.version);
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
			// v16: also accept real bidi streams (for SubscribeNamespace)
			if (this.version === Version.DRAFT_16) {
				void this.#acceptNativeBidis();
			}

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

	/** Accept real WebTransport bidi streams and queue them for acceptBi (v16). */
	async #acceptNativeBidis(): Promise<void> {
		try {
			for (;;) {
				const stream = await Stream.accept(this.#quic, this.version);
				if (!stream) break;

				const waiter = this.#incomingWaiters.shift();
				if (waiter) {
					waiter(stream);
				} else {
					this.#incomingQueue.push(stream);
				}
			}
		} catch {
			// Session closed
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
		console.debug(`adapter: closing stream requestId=${requestId}`);
		this.#streams.delete(requestId);
		this.#subscribeNamespaces.delete(requestId);
		const namespace = this.#namespacesByRequestId.get(requestId);
		if (namespace !== undefined) {
			this.#namespaces.delete(namespace);
			this.#namespacesByRequestId.delete(requestId);
		}
		try {
			entry.controller.close();
		} catch {
			// Already closed
		}
	}

	/**
	 * Returns the total byte size of the first complete message in buffer,
	 * or undefined if the buffer doesn't contain a complete message yet.
	 * Message format: [typeId varint][size u16 BE][body of `size` bytes]
	 */
	#messageSize(buffer: Uint8Array): number | undefined {
		if (buffer.length === 0) return undefined;

		const typeSize = 1 << ((buffer[0] & 0xc0) >> 6);
		if (buffer.length < typeSize) return undefined;

		const [, afterType] = Varint.decode(buffer);
		if (afterType.length < 2) return undefined;

		const size = (afterType[0] << 8) | afterType[1];
		const totalSize = buffer.length - afterType.length + 2 + size;
		if (buffer.length < totalSize) return undefined;

		return totalSize;
	}

	/**
	 * Try to parse the first outgoing message from accumulated bytes.
	 * Returns the requestId if enough data is available, undefined otherwise.
	 */
	#tryParseOutgoing(buffer: Uint8Array): { requestId: bigint } | undefined {
		if (buffer.length === 0) return undefined;

		// Check typeId varint size before decoding
		const typeSize = 1 << ((buffer[0] & 0xc0) >> 6);
		if (buffer.length < typeSize) return undefined;

		const [typeId, afterType] = Varint.decode(buffer);

		// Need 2 bytes for u16 size
		if (afterType.length < 2) return undefined;

		const size = (afterType[0] << 8) | afterType[1];
		const bodyStart = afterType.subarray(2);

		// Need full body
		if (bodyStart.length < size) return undefined;

		const body = bodyStart.subarray(0, size);

		// Decode requestId from body (QUIC varint)
		const [reqId] = Varint.decode(body);
		const requestId = BigInt(reqId);

		// PublishNamespace (0x06): also parse namespace for v14/v15 reverse lookup
		if (typeId === 0x06) {
			try {
				const [, afterReqId] = Varint.decode(body);
				this.#parseAndRegisterNamespace(afterReqId, requestId);
			} catch {
				// Non-critical: only needed for v14/v15 PublishNamespaceDone/Cancel
			}
		}

		// SubscribeNamespace (0x11): register for follow-up routing
		if (typeId === 0x11) {
			this.#subscribeNamespaces.add(requestId);
		}

		return { requestId };
	}

	/**
	 * Parse a namespace from raw bytes and register it for reverse lookup.
	 */
	#parseAndRegisterNamespace(buf: Uint8Array, requestId: bigint) {
		const decoder = new TextDecoder();
		const [partCount, afterCount] = Varint.decode(buf);
		let cursor = afterCount;
		const parts: string[] = [];
		for (let i = 0; i < partCount; i++) {
			const [len, afterLen] = Varint.decode(cursor);
			parts.push(decoder.decode(afterLen.subarray(0, len)));
			cursor = afterLen.subarray(len);
		}
		const namespace = parts.join("/");
		this.#namespaces.set(namespace, requestId);
		this.#namespacesByRequestId.set(requestId, namespace);
	}

	/** Create a WritableStream that buffers and writes complete messages to the control stream under mutex. */
	#createSendWritable(): WritableStream<Uint8Array> {
		let buffer = new Uint8Array(0);
		return new WritableStream<Uint8Array>({
			write: async (chunk) => {
				const newBuf = new Uint8Array(buffer.length + chunk.length);
				newBuf.set(buffer);
				newBuf.set(chunk, buffer.length);
				buffer = newBuf;

				for (;;) {
					const boundary = this.#messageSize(buffer);
					if (boundary === undefined) break;

					const toFlush = buffer.subarray(0, boundary);
					buffer = buffer.subarray(boundary);
					await this.#writeMutex.runExclusive(() => this.#writer.write(toFlush));
				}
			},
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
			// === FollowUp: route to existing stream ===
			case 0x02: {
				// SubscribeUpdate / REQUEST_UPDATE
				const requestId = await readRequestId();
				return { route: Route.FollowUp, requestId };
			}

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
				this.#namespacesByRequestId.set(requestId, namespace);
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
				if (this.version === Version.DRAFT_14) {
					// PublishNamespaceError
					const requestId = await readRequestId();
					return { route: Route.ErrorResponse, requestId };
				}
				// v15: Namespace entry (no requestId) — route to SubscribeNamespace stream
				const subNs08 = this.#subscribeNamespaces.values().next().value;
				if (subNs08 === undefined) throw new Error("unexpected message 0x08: no SubscribeNamespace stream");
				return { route: Route.FollowUp, requestId: subNs08 };
			}
			case 0x0e: {
				// v15: NamespaceDone entry (no requestId) — route to SubscribeNamespace stream
				const subNs0e = this.#subscribeNamespaces.values().next().value;
				if (subNs0e === undefined) throw new Error("unexpected message 0x0e: no SubscribeNamespace stream");
				return { route: Route.FollowUp, requestId: subNs0e };
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
		console.debug("adapter: close() called");

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
		this.#namespacesByRequestId.clear();
		this.#subscribeNamespaces.clear();

		// Unblock maxRequestId waiters
		for (const resolve of this.#maxRequestIdResolves) resolve();
		this.#maxRequestIdResolves = [];
	}
}

import { Once } from "@moq/signals";
import { Mutex } from "async-mutex";
import type { Drain } from "../connection/goaway.ts";
import { error, ProtocolViolation, SessionCode, SessionError } from "../error.ts";
import { Reader, Stream, type Writer } from "../stream.ts";
import { untilAborted } from "../util/abort.ts";
import * as Varint from "../varint.ts";
import { GoAway } from "./goaway.ts";
import * as Namespace from "./namespace.ts";
import { initialMaxRequestId, MaxRequestId, REQUEST_WINDOW } from "./request.ts";
import { type IetfVersion, Version } from "./version.ts";

/** The peer used a request ID the draft closes the session for. */
export class RequestWindowError extends Error {
	readonly code: number;

	constructor(code: number, message: string) {
		super(message);
		this.name = "RequestWindowError";
		this.code = code;
	}
}

/**
 * Interface for opening outgoing bidi streams and allocating request IDs.
 * Implemented by both ControlStreamAdapter (v14-v16) and NativeSession (v17).
 */
export interface Session {
	openBi(): Stream | Promise<Stream>;
	openNativeBi?(): Promise<Stream>;
	acceptBi(): Promise<Stream | undefined>;
	nextRequestId(signal?: AbortSignal): Promise<bigint | undefined>;
	/** Spend a request ID the peer used without holding a request open, closing the session if it cannot. */
	spendRequest(id: bigint): void;
	close(): void;
	readonly version: IetfVersion;
}

/**
 * v17 native session — thin wrapper around WebTransport.
 * Each request gets its own real bidi stream; no control stream multiplexing.
 */
export class NativeSession implements Session {
	#quic: WebTransport;
	// moq-transport reserves even request IDs for the client and odd for the server,
	// so the two peers' ID spaces never overlap.
	#requestId: bigint;
	// The peer's IDs, of the other parity. Draft 17 has no window, yet an ID stays spent.
	#spent: SpentIds;
	readonly version: IetfVersion;

	constructor(quic: WebTransport, version: IetfVersion, client: boolean) {
		this.#quic = quic;
		this.version = version;
		this.#requestId = client ? 0n : 1n;
		this.#spent = new SpentIds(client ? 1n : 0n);
	}

	spendRequest(id: bigint): void {
		try {
			this.#spent.spend(id);
		} catch (err) {
			if (err instanceof SessionError) this.#quic.close({ closeCode: err.code, reason: err.message });
			throw err;
		}
	}

	async openBi(): Promise<Stream> {
		return Stream.open(this.#quic, { version: this.version });
	}

	async acceptBi(): Promise<Stream | undefined> {
		return Stream.accept(this.#quic, this.version);
	}

	async nextRequestId(signal?: AbortSignal): Promise<bigint | undefined> {
		signal?.throwIfAborted();
		const id = this.#requestId;
		this.#requestId += 2n;
		return id;
	}

	/** Closes the underlying WebTransport session. */
	close() {
		this.#quic.close();
	}
}

// Route classification for control stream messages.
const Route = {
	NewRequest: 0, // Create virtual bidi stream, push initial message
	Response: 1, // Push message to existing stream (keep open)
	ErrorResponse: 2, // Push a final message to existing stream, then close
	CloseStream: 3, // Close stream recv (no bytes pushed)
	FollowUp: 4, // Push follow-up message to existing stream
	MaxRequestId: 5, // Update flow control
	Ignore: 6, // Connection-level, no routing
	GoAway: 7, // Terminal
	Update: 8, // Spend the update's own request ID, then follow up on its target
} as const;
type Route = (typeof Route)[keyof typeof Route];

interface StreamEntry {
	controller: ReadableStreamDefaultController<Uint8Array>;
}

type NamespaceDirection = "incoming" | "outgoing";

/** Runs per chunk before it splits, so a split or removal shifts at most one chunk. */
const RUN_CHUNK = 256;

/**
 * Skipped request IDs as sorted, disjoint `[start, end)` runs of one parity.
 *
 * A jump costs one run however far it skips, and only an ID landing inside a run splits it,
 * so the runs never outnumber the requests received. They are kept in chunks so a split
 * costs the chunk it lands in rather than shifting the whole table.
 */
class MissingRuns {
	#chunks: [bigint, bigint][][] = [];
	#size = 0;

	/** How many runs are remembered. */
	get size(): number {
		return this.#size;
	}

	/** Whether `id` was skipped and not yet received. */
	has(id: bigint): boolean {
		return this.#find(id) !== undefined;
	}

	/** Whether taking `id` leaves one more run, landing strictly inside its run. */
	splits(id: bigint): boolean {
		const found = this.#find(id);
		if (!found) return false;
		const [start, end] = this.#chunks[found[0]][found[1]];
		return start < id && id + 2n < end;
	}

	/** Mark a skipped `id` received, splitting its run. */
	take(id: bigint): void {
		const found = this.#find(id);
		if (!found) return;
		const [index, run] = found;
		const chunk = this.#chunks[index];
		const [start, end] = chunk[run];
		const rest: [bigint, bigint][] = [];
		if (start < id) rest.push([start, id]);
		if (id + 2n < end) rest.push([id + 2n, end]);
		this.#size += rest.length - 1;
		chunk.splice(run, 1, ...rest);
		if (chunk.length === 0) this.#chunks.splice(index, 1);
		else if (chunk.length > 2 * RUN_CHUNK) this.#chunks.splice(index + 1, 0, chunk.splice(RUN_CHUNK));
	}

	/** Record `[start, end)` as skipped, above every existing run. */
	push(start: bigint, end: bigint): void {
		this.#size++;
		const last = this.#chunks.at(-1);
		if (last && last.length < 2 * RUN_CHUNK) last.push([start, end]);
		else this.#chunks.push([[start, end]]);
	}

	/** The chunk and run holding `id`. */
	#find(id: bigint): [number, number] | undefined {
		const index = lastAtOrBelow(this.#chunks, id, (chunk) => chunk[0][0]);
		if (index < 0) return undefined;
		const chunk = this.#chunks[index];
		const run = lastAtOrBelow(chunk, id, (run) => run[0]);
		return run >= 0 && id < chunk[run][1] ? [index, run] : undefined;
	}
}

/**
 * The peer's spent request IDs on draft 17 and later: every ID of its parity below `#next`,
 * except the skipped runs. Mirrors Rust's `Spent`.
 */
export class SpentIds {
	#next: bigint;
	#missing = new MissingRuns();
	// No window bounds the runs here, and forgetting one would make its IDs usable again.
	#limit: number;

	constructor(first: bigint, limit = Number(REQUEST_WINDOW)) {
		this.#next = first;
		this.#limit = limit;
	}

	/** Spend `id`, throwing the session error a reused, wrong-parity, or unrecordable ID closes with. */
	spend(id: bigint): void {
		// No peer reaches 2^62 requests; refusing past it matches Rust, whose `next` would overflow.
		const unused = id < 1n << 62n && (id >= this.#next || this.#missing.has(id));
		if (id % 2n !== this.#next % 2n || !unused) throw new SessionError(SessionCode.InvalidRequestId);
		const splits = id < this.#next ? this.#missing.splits(id) : id > this.#next;
		if (splits && this.#missing.size >= this.#limit) {
			throw new SessionError(SessionCode.Internal, { reason: "request ID history exhausted" });
		}
		if (id < this.#next) {
			this.#missing.take(id);
			return;
		}
		if (id > this.#next) this.#missing.push(this.#next, id);
		this.#next = id + 2n;
	}
}

/** The last index whose key is at or below `id`, or -1, in an array sorted by key. */
function lastAtOrBelow<T>(items: T[], id: bigint, key: (item: T) => bigint): number {
	let lo = 0;
	let hi = items.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (key(items[mid]) <= id) lo = mid + 1;
		else hi = mid;
	}
	return lo - 1;
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

	/** The peer's GOAWAY, which draft-14 to -16 carry on the shared control stream. */
	readonly goaway = new Once<Drain>();

	// Virtual streams keyed by requestId
	#streams = new Map<bigint, StreamEntry>();

	// Namespace → requestId reverse lookups for v14/v15 namespace-keyed messages.
	// DONE names an incoming announcement and CANCEL names an outgoing one, so a relay
	// may legitimately hold the same namespace in both maps at once.
	#incomingNamespaces = new Map<string, bigint>();
	#outgoingNamespaces = new Map<string, bigint>();

	// requestId → namespace reverse lookup, one entry per request including a refused
	// duplicate, so #forget can clean up whichever request goes away.
	#namespacesByRequestId = new Map<bigint, { namespace: string; direction: NamespaceDirection }>();

	// SubscribeNamespace requestIds — for routing 0x08/0x0E entries that lack requestId (v14/v15)
	#subscribeNamespaces = new Set<bigint>();

	// Incoming stream queue (for acceptBi)
	#incomingQueue: Stream[] = [];
	#incomingWaiters: ((stream: Stream | undefined) => void)[] = [];

	// Request ID flow control. moq-transport reserves even request IDs for the
	// client and odd for the server, so the two peers' ID spaces never overlap.
	// This matters here because a single `#streams` map routes every request by
	// ID: overlapping spaces would let an inbound request clobber the routing
	// entry of an outbound one with the same number (e.g. an inbound
	// PUBLISH_NAMESPACE stealing a pending SUBSCRIBE's slot, so SUBSCRIBE_OK is
	// delivered to the wrong virtual stream).
	#requestId: bigint;
	#maxRequestId: bigint;
	#maxRequestIdResolves = new Set<() => void>();

	// Drafts 14/15 require our new request IDs in order on the control stream. Callers
	// allocate an ID and then encode asynchronously, so a shorter later request can finish
	// first: a message spending a new ID waits here until every lower one has gone out.
	// Draft 16 sends SUBSCRIBE_NAMESPACE on its own stream and the peer accepts any order.
	#sendNext: bigint | undefined;
	#sendTurns = new Map<bigint, () => void>();

	// The peer's requests, flow controlled as moq-net does: at most `#peerWindow` open at
	// once, every ID below `#peerMax`, and once half the window has closed the bound rises
	// by the IDs they spent and goes out as one MAX_REQUEST_ID.
	#peerWindow: bigint;
	#peerMax: bigint;
	#peerNext: bigint;
	// Draft 16 reorders across streams, so it remembers the IDs skipped below #peerNext.
	#peerMissing: MissingRuns | undefined;
	#open = new Set<bigint>();
	#retired = 0n;
	#granting = false;
	#grantDirty = false;

	#closed = false;

	// Whether this side opened the session. Only a server may name a redirect.
	#client: boolean;

	constructor(
		quic: WebTransport,
		controlStream: Stream,
		version: IetfVersion,
		maxRequestId: bigint,
		client: boolean,
		window: bigint = REQUEST_WINDOW,
	) {
		// Routing assumes the shared control stream's layout, such as an update's two ids.
		if (version >= Version.DRAFT_17)
			throw new Error(`control stream adapter is for drafts 14 to 16, not ${version}`);
		this.#quic = quic;
		this.#reader = controlStream.reader;
		this.#reader.version = version;
		this.#writer = controlStream.writer;
		this.#writer.version = version;
		this.version = version;
		this.#maxRequestId = maxRequestId;
		this.#peerWindow = window;
		this.#peerMax = initialMaxRequestId(!client, window);
		this.#peerNext = client ? 1n : 0n;
		this.#peerMissing = version === Version.DRAFT_16 ? new MissingRuns() : undefined;
		this.#requestId = client ? 0n : 1n;
		this.#sendNext = version === Version.DRAFT_16 ? undefined : this.#requestId;
		this.#client = client;
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
					this.#forget(registeredRequestId);
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

					let spent: bigint | undefined;
					if (!registered) {
						// First message: extract requestId and register before flushing
						const parsed = await this.#tryParseOutgoing(toFlush);
						if (parsed) {
							registeredRequestId = parsed.requestId;
							this.#streams.set(parsed.requestId, { controller });
							registered = true;
							spent = parsed.requestId;
						}
					} else {
						spent = this.#updateId(toFlush);
					}

					await this.#send(toFlush, spent);
				}
			},
		});

		const stream = new Stream({ readable, writable: sendWritable, version: this.version });
		return stream;
	}

	/**
	 * Open a real WebTransport bidi stream (for v16 SubscribeNamespace).
	 */
	async openNativeBi(): Promise<Stream> {
		return Stream.open(this.#quic, { version: this.version });
	}

	/**
	 * Take a slot for a request that arrived on its own stream (draft-16 SUBSCRIBE_NAMESPACE).
	 * Control-stream requests are admitted while they are classified.
	 */
	admitRequest(requestId: bigint): void {
		this.#admitHeld(requestId);
	}

	/** Give back a slot taken by {@link admitRequest}. */
	releaseRequest(requestId: bigint): void {
		if (this.#open.delete(requestId)) this.#retire();
	}

	/**
	 * Allocate the next request ID, canceling before allocation while waiting for credit.
	 * A returned legacy ID is spent and its request must be sent, even if then canceled.
	 */
	async nextRequestId(signal?: AbortSignal): Promise<bigint | undefined> {
		for (;;) {
			signal?.throwIfAborted();
			if (this.#closed) return undefined;
			const id = this.#requestId;
			if (id < this.#maxRequestId) {
				this.#requestId += 2n;
				return id;
			}
			const { promise, resolve } = Promise.withResolvers<void>();
			this.#maxRequestIdResolves.add(resolve);
			try {
				await untilAborted(promise, signal);
			} finally {
				this.#maxRequestIdResolves.delete(resolve);
			}
		}
	}

	/**
	 * Main run loop — reads control stream messages and routes to virtual streams.
	 * Must be called after construction. Runs until the control stream closes.
	 */
	async run(): Promise<void> {
		// Why the virtual streams end: undefined only for a GOAWAY, which is not a failure.
		let cause: Error | undefined;
		try {
			// v16: also accept real bidi streams (for SubscribeNamespace)
			if (this.version === Version.DRAFT_16) {
				void this.#acceptNativeBidis();
			}

			for (;;) {
				const done = await this.#reader.done();
				if (done) {
					cause = new ProtocolViolation("control stream closed");
					break;
				}

				const typeId = await this.#reader.u53();
				const size = await this.#reader.u16();
				const body = await this.#reader.read(size);

				const classified = await this.#classify(typeId, body);

				if (classified.route === Route.GoAway) {
					// The session keeps serving: a GOAWAY asks us to migrate, not to stop reading.
					const msg = await GoAway.decodeBody(body, this.version);
					if (this.goaway.peek() !== undefined) throw new ProtocolViolation("duplicate GOAWAY");
					// A client may leave, but only the server may name where to go.
					if (!this.#client && msg.newSessionUri !== "") {
						throw new ProtocolViolation("client GOAWAY must not name a redirect");
					}
					this.goaway.set(msg.drain());
					continue;
				}

				if (classified.route === Route.Update) {
					this.spendRequest(classified.ownRequestId);
					this.#pushMessage(classified.requestId, typeId, size, body);
					continue;
				}

				const { route, requestId } = classified;

				switch (route) {
					case Route.NewRequest:
						this.#admitHeld(requestId);
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
						this.#maxRequestIdResolves.clear();
						break;
				}
			}
		} catch (err: unknown) {
			cause = error(err);
			throw err;
		} finally {
			this.close(cause);
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
				this.#forget(requestId);
			},
		});

		const sendWritable = this.#createSendWritable();

		const stream = new Stream({ readable, writable: sendWritable, version: this.version });

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
		this.#forget(requestId);
		if (!entry) return;
		console.debug(`adapter: closing stream requestId=${requestId}`);
		try {
			entry.controller.close();
		} catch {
			// Already closed
		}
	}

	/**
	 * Drop every routing entry a request owns, keeping the two namespace maps in step.
	 *
	 * Runs even when the virtual stream is already gone, so a request that was torn down
	 * locally still gives its namespace back.
	 */
	#forget(requestId: bigint) {
		const held = this.#open.delete(requestId);
		this.#streams.delete(requestId);
		this.#subscribeNamespaces.delete(requestId);

		const registered = this.#namespacesByRequestId.get(requestId);
		if (registered !== undefined) {
			this.#namespacesByRequestId.delete(requestId);
			const namespaces = this.#namespaceRequests(registered.direction);

			// Only the request holding the live announcement may retract it. A refused duplicate
			// carries the same name, and letting its close withdraw the name would strand the
			// original: its own DONE would then resolve to nothing.
			if (namespaces.get(registered.namespace) === requestId) namespaces.delete(registered.namespace);
		}

		if (held) this.#retire();
	}

	/** Reject a request ID the peer is not allowed to use. */
	#checkPeer(requestId: bigint): void {
		if (requestId >= this.#peerMax) {
			// Section 9.1 also names INVALID_REQUEST_ID here, but the session registry and
			// section 9.5 assign an ID past the maximum to TOO_MANY_REQUESTS.
			this.#refuse(SessionCode.TooManyRequests, "request id exceeds max");
		}
		// Even IDs are the client's. `#client` is this side, so the peer's IDs are the other parity.
		const even = (requestId & 1n) === 0n;
		if (even === this.#client) {
			this.#refuse(SessionCode.InvalidRequestId, "request id has the wrong parity");
		}
	}

	#admitHeld(requestId: bigint): void {
		this.#checkPeer(requestId);
		const unused =
			requestId < this.#peerNext
				? this.#peerMissing?.has(requestId) === true
				: requestId === this.#peerNext || this.#peerMissing !== undefined;
		if (!unused) {
			this.#refuse(SessionCode.InvalidRequestId, "request id was already spent or out of sequence");
		}
		this.#checkLive();
		if (requestId < this.#peerNext) this.#peerMissing?.take(requestId);
		else if (requestId > this.#peerNext) this.#peerMissing?.push(this.#peerNext, requestId);
		if (requestId >= this.#peerNext) this.#peerNext = requestId + 2n;
		this.#open.add(requestId);
	}

	/**
	 * An update spends its own request ID like any request but keeps no stream, so the
	 * slot is retired at once. The ID is spent even when the target is already gone.
	 */
	spendRequest(requestId: bigint): void {
		this.#admitHeld(requestId);
		this.#open.delete(requestId);
		this.#retire();
	}

	/** Enforce the live-request cap independently of the ID bound. */
	#checkLive(): void {
		if (BigInt(this.#open.size) >= this.#peerWindow) {
			this.#refuse(SessionCode.TooManyRequests, "too many open requests");
		}
	}

	#refuse(code: number, message: string): never {
		try {
			this.#quic.close({ closeCode: code, reason: message });
		} catch {
			// The session is already closed.
		}
		throw new RequestWindowError(code, message);
	}

	/**
	 * Count a request that ended. Once half the window has, raise the bound by the IDs
	 * they spent and send it, so grants cost a fraction of the requests they admit.
	 */
	#retire(): void {
		this.#retired += 1n;
		if (this.#retired * 2n < this.#peerWindow) return;
		this.#peerMax += this.#retired * 2n;
		this.#retired = 0n;
		this.#grantDirty = true;
		if (this.#granting || this.#closed) return;
		this.#granting = true;
		void this.#flushGrant();
	}

	async #flushGrant(): Promise<void> {
		try {
			while (this.#grantDirty && !this.#closed) {
				this.#grantDirty = false;
				const max = this.#peerMax;
				await this.#writeMutex.runExclusive(async () => {
					if (this.#closed) return;
					await this.#writer.u53(MaxRequestId.id);
					await new MaxRequestId({ requestId: max }).encode(this.#writer, this.version);
				});
			}
		} catch {
			// The control stream closed under the grant.
		} finally {
			this.#granting = false;
		}
		if (this.#grantDirty && !this.#closed) {
			this.#granting = true;
			void this.#flushGrant();
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
	async #tryParseOutgoing(buffer: Uint8Array): Promise<{ requestId: bigint } | undefined> {
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
				await this.#registerNamespace(body, "outgoing");
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
	 * Decode a PublishNamespace body and register its namespace for the v14/v15
	 * namespace-keyed withdrawals. Returns the request ID it carried.
	 *
	 * The one decode site for both directions: outgoing announcements go through
	 * #tryParseOutgoing and incoming ones through #classify. The direction selects the
	 * map that the peer's DONE or CANCEL will later consult.
	 */
	async #registerNamespace(body: Uint8Array, direction: NamespaceDirection): Promise<bigint> {
		const r = new Reader(undefined, body, this.version);
		const requestId = await r.u62();
		const namespace = await Namespace.decode(r);
		const namespaces = this.#namespaceRequests(direction);

		this.#namespacesByRequestId.set(requestId, { namespace, direction });

		// First announcement in this direction wins. A duplicate is refused with 409, and
		// overwriting here would point the first request's withdrawal at the refused one,
		// which has no stream left, leaving the namespace announced forever.
		if (!namespaces.has(namespace)) namespaces.set(namespace, requestId);

		return requestId;
	}

	#namespaceRequests(direction: NamespaceDirection): Map<string, bigint> {
		return direction === "incoming" ? this.#incomingNamespaces : this.#outgoingNamespaces;
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
					await this.#send(toFlush, this.#updateId(toFlush));
				}
			},
		});
	}

	/** The update's own request ID if this complete message is SUBSCRIBE_UPDATE or REQUEST_UPDATE. */
	#updateId(message: Uint8Array): bigint | undefined {
		const [typeId, afterType] = Varint.decode(message);
		if (typeId !== 0x02) return undefined;
		return Varint.decodeBigInt(afterType.subarray(2))[0];
	}

	/** Write one complete message, after every lower request ID when it spends a new one. */
	async #send(message: Uint8Array, requestId: bigint | undefined): Promise<void> {
		if (requestId === undefined || this.#sendNext === undefined) {
			await this.#writeMutex.runExclusive(() => this.#writer.write(message));
			return;
		}
		if (requestId < this.#sendNext || this.#sendTurns.has(requestId)) {
			throw new Error(`request id ${requestId} was already sent`);
		}
		if (requestId !== this.#sendNext) {
			const { promise, resolve } = Promise.withResolvers<void>();
			this.#sendTurns.set(requestId, resolve);
			await promise;
			if (this.#closed) throw new Error("control stream closed");
		}
		try {
			await this.#writeMutex.runExclusive(() => this.#writer.write(message));
		} finally {
			this.#sendNext = requestId + 2n;
			const next = this.#sendTurns.get(this.#sendNext);
			this.#sendTurns.delete(this.#sendNext);
			next?.();
		}
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
	): Promise<
		| { route: typeof Route.GoAway }
		| { route: typeof Route.Update; ownRequestId: bigint; requestId: bigint }
		| { route: Exclude<Route, typeof Route.GoAway | typeof Route.Update>; requestId: bigint }
	> {
		const readRequestId = async (): Promise<bigint> => {
			const r = new Reader(undefined, body, this.version);
			return await r.u62();
		};

		// v14/v15 name their withdrawals instead of numbering them. A name we have no
		// announcement for is dropped, not fatal: it belongs to a duplicate we already
		// refused, or to a request that is already gone, so there is nothing left to close.
		// The mapping itself is released by #forget when the stream goes.
		const readNamespaceRequestId = async (direction: NamespaceDirection): Promise<bigint | undefined> => {
			const r = new Reader(undefined, body, this.version);
			const namespace = await Namespace.decode(r);
			const namespaces = this.#namespaceRequests(direction);
			const requestId = namespaces.get(namespace);
			if (requestId === undefined) console.warn(`adapter: no announcement for namespace: ${namespace}`);
			return requestId;
		};

		switch (typeId) {
			// === FollowUp: route to existing stream ===
			case 0x02: {
				// SUBSCRIBE_UPDATE / REQUEST_UPDATE: the update's own Request ID, then the request
				// it changes (Subscription Request ID on 14 and 15, Existing Request ID on 16).
				const r = new Reader(undefined, body, this.version);
				const ownRequestId = await r.u62();
				const requestId = await r.u62();
				return { route: Route.Update, ownRequestId, requestId };
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
				// PublishNamespace: also store the namespace for v14/v15 reverse lookup
				const requestId = await this.#registerNamespace(body, "incoming");
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
				if (this.version === Version.DRAFT_14 || this.version === Version.DRAFT_15) {
					throw new Error("unexpected message 0x0e");
				}
				// v16+: NamespaceDone entry (no requestId) — route to SubscribeNamespace stream
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
				// PublishDone: the subscriber reads its status and stream count before the end.
				const requestId = await readRequestId();
				return { route: Route.ErrorResponse, requestId };
			}
			case 0x17: {
				// FetchCancel
				const requestId = await readRequestId();
				return { route: Route.CloseStream, requestId };
			}
			case 0x09: {
				// PublishNamespaceDone: v16 uses requestId, v14/v15 names the incoming
				// announcement its sender is withdrawing.
				if (this.version === Version.DRAFT_16) {
					const requestId = await readRequestId();
					return { route: Route.CloseStream, requestId };
				}
				const requestId = await readNamespaceRequestId("incoming");
				if (requestId === undefined) return { route: Route.Ignore, requestId: 0n };
				return { route: Route.CloseStream, requestId };
			}
			case 0x0c: {
				// PublishNamespaceCancel: v16 uses requestId, v14/v15 names the outgoing
				// announcement its sender is rejecting.
				if (this.version === Version.DRAFT_16) {
					const requestId = await readRequestId();
					return { route: Route.CloseStream, requestId };
				}
				const requestId = await readNamespaceRequestId("outgoing");
				if (requestId === undefined) return { route: Route.Ignore, requestId: 0n };
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

	/**
	 * Ends every virtual stream: cleanly for a deliberate close, or with `err` when the
	 * control stream died under them, since every request riding it was cut off.
	 */
	close(err?: Error) {
		if (this.#closed) return;
		this.#closed = true;
		console.debug("adapter: close() called");

		for (const entry of this.#streams.values()) {
			try {
				if (err) entry.controller.error(err);
				else entry.controller.close();
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
		this.#incomingNamespaces.clear();
		this.#outgoingNamespaces.clear();
		this.#namespacesByRequestId.clear();
		this.#subscribeNamespaces.clear();

		// Unblock maxRequestId waiters
		for (const resolve of this.#maxRequestIdResolves) resolve();
		this.#maxRequestIdResolves.clear();

		for (const resolve of this.#sendTurns.values()) resolve();
		this.#sendTurns.clear();
	}
}

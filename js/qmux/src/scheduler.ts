/** Prioritized send scheduler for a QMux session.
 *
 * A single writer loop owns the socket. Frames are written in priority order:
 * control frames (flow control, resets, pings, close) always preempt stream
 * data, and among stream data the highest `sendOrder` wins (W3C WebTransport
 * convention — larger value is sent first), with round-robin among equal
 * priorities. Prioritization only bites under backpressure; when the sink has
 * room every frame is written as soon as it's queued.
 *
 * The backpressure source is abstracted behind {@link SendSink} so we can use
 * `WebSocketStream`'s real (event-driven) backpressure where available and fall
 * back to polling `WebSocket.bufferedAmount` everywhere else.
 */

/** A write sink over the underlying socket, exposing backpressure. */
export interface SendSink {
	/** Resolve once the sink can accept another write without exceeding its
	 *  high-water mark. Rejects if the socket is closed/errored. */
	ready(): Promise<void>;
	/** Write bytes to the socket, resolving once the write is accepted. Rejects
	 *  if the write fails — the scheduler must observe this rather than resolve
	 *  the frame's promise as if it succeeded. */
	write(bytes: Uint8Array): Promise<void>;
	/** Synchronous backpressure probe: true when the sink can take a write now
	 *  without buffering past its high-water mark. Used to drop datagrams the
	 *  instant the transport starts queuing, rather than letting them pile up
	 *  behind a stream/control backlog and arrive stale. */
	wantsMore(): boolean;
}

/** Backpressure-aware sink over a `WebSocketStream` writable. The writable's
 *  `writer.ready` is the backpressure signal — native `WebSocketStream` ties it
 *  to the real send buffer, while the `WebSocketStream` ponyfill drives
 *  it from `bufferedAmount` against a (resizable) high-water mark. */
export class WritableStreamSink implements SendSink {
	#writer: WritableStreamDefaultWriter<Uint8Array>;

	constructor(writable: WritableStream<Uint8Array>) {
		this.#writer = writable.getWriter();
	}

	ready(): Promise<void> {
		return this.#writer.ready;
	}

	write(bytes: Uint8Array): Promise<void> {
		// Propagate failures so the scheduler can reject the frame's promise.
		return this.#writer.write(bytes);
	}

	wantsMore(): boolean {
		// desiredSize > 0 means below the high-water mark (room to write); 0 or
		// negative means backpressured; null means the stream errored/closed.
		// Native WebSocketStream ties this to the real send buffer; the ponyfill
		// derives it from bufferedAmount, so both report true backpressure.
		const desired = this.#writer.desiredSize;
		return desired !== null && desired > 0;
	}
}

/** One stream's single in-flight frame, awaiting its turn on the wire. */
interface Waiter {
	seq: number;
	bytes: Uint8Array;
	resolve: () => void;
	reject: (err: Error) => void;
}

export interface SchedulerOptions {
	/** The connection's opening frame, written before anything else. A close
	 *  never drops it, since the peer rejects any other frame first. */
	first?: Uint8Array;
	/** Called after each frame is actually written (to track send activity). */
	onActivity?: () => void;
	/** Called once if the sink fails, after which nothing more is sent. */
	onFail?: (err: Error) => void;
}

/** Default `sendOrder` for streams that don't set one. */
export const DEFAULT_SEND_ORDER = 0;

/** How many outbound datagrams to buffer before dropping. Datagrams are
 *  unreliable, so a backpressured transport sheds them here rather than growing
 *  an unbounded queue. */
const DATAGRAM_QUEUE_LIMIT = 1024;

export class SendScheduler {
	#sink: SendSink;
	#onActivity: () => void;
	#onFail: (err: Error) => void;
	#closed?: Error;

	// Control frames preempt all stream data, FIFO among themselves. Lossless and
	// uncapped, so whatever the peer can trigger here must be bounded by design:
	// at most one RESET_STREAM or STOP_SENDING per stream, and flow-control and
	// stream-count updates only as the peer spends credit it has already received
	// (which a peer not reading our writes never gets more of).
	#control: Uint8Array[] = [];

	// Control frames where only the newest matters (QX_PING requests and
	// responses), keyed by kind. A newer frame replaces one still unsent, so a peer
	// that pings faster than it reads our responses can't grow this lane.
	#latest = new Map<string, Uint8Array>();

	// Datagrams: serviced after control, ahead of stream data. Bounded and lossy
	// (unlike #control) since unreliable datagrams are meant to be droppable —
	// shed on transport backpressure or a full lane.
	#datagrams: Uint8Array[] = [];

	// Per-stream send priority and the single pending frame per ready stream.
	// Invariant: a stream has at most one Waiter at a time, because its writer
	// task awaits each enqueue before producing the next frame. This keeps
	// per-stream byte order (offsets) sequential.
	#sendOrders = new Map<bigint, number>();
	#ready = new Map<bigint, Waiter>();
	#seq = 0;

	// Resolver for the loop when it's parked with no work.
	#wake?: () => void;
	// Settles when the writer loop exits.
	#done = Promise.withResolvers<void>();

	constructor(sink: SendSink, options?: SchedulerOptions) {
		this.#sink = sink;
		this.#onActivity = options?.onActivity ?? (() => {});
		this.#onFail = options?.onFail ?? (() => {});
		// Start the writer loop. It never rejects — failures are captured by #fail.
		void this.#run(options?.first);
	}

	/** Queue a pre-encoded control frame. Preempts all stream data. */
	enqueueControl(bytes: Uint8Array): void {
		if (this.#closed) throw this.#closed;
		this.#control.push(bytes);
		this.#signal();
	}

	/** Queue a control frame that supersedes any unsent one with the same `key`.
	 *  Serviced after the FIFO control lane, ahead of datagrams and stream data. */
	replaceControl(key: string, bytes: Uint8Array): void {
		if (this.#closed) throw this.#closed;
		this.#latest.set(key, bytes);
		this.#signal();
	}

	/** Queue a datagram frame — best-effort, serviced after control but ahead of
	 *  stream data. Dropped (rather than queued) when:
	 *   - the scheduler is closed;
	 *   - the transport is backpressured — the socket is already queuing stream/
	 *     control data, so a datagram behind it would arrive stale; or
	 *   - the datagram lane is full, bounding a synchronous burst that outruns the
	 *     writer loop even while the transport has room.
	 *  An unreliable datagram is meant to be droppable, so this sheds rather than
	 *  applying backpressure to the caller. */
	enqueueDatagram(bytes: Uint8Array): void {
		if (this.#closed) return;
		if (!this.#sink.wantsMore()) return; // transport is queuing — drop, don't delay
		if (this.#datagrams.length >= DATAGRAM_QUEUE_LIMIT) return; // burst cap
		this.#datagrams.push(bytes);
		this.#signal();
	}

	/** Set (or update) a stream's send priority. Takes effect immediately,
	 *  including for the stream's already-queued frame (priority is read at
	 *  selection time), so promoting a stream lets it jump a lower-priority
	 *  backlog without reordering its own bytes. */
	setSendOrder(streamId: bigint, order: number): void {
		this.#sendOrders.set(streamId, order);
	}

	/** Queue a stream-data frame. Resolves once the bytes hit the socket;
	 *  rejects if the session closes or the stream is dropped first. */
	enqueueStream(streamId: bigint, bytes: Uint8Array): Promise<void> {
		if (this.#closed) return Promise.reject(this.#closed);
		// Enforce the one-frame-per-stream invariant: a producer must await each
		// enqueue before the next. A duplicate would orphan the previous promise.
		if (this.#ready.has(streamId)) {
			return Promise.reject(new Error(`stream ${streamId} already has a queued frame`));
		}
		return new Promise<void>((resolve, reject) => {
			this.#ready.set(streamId, { seq: this.#seq++, bytes, resolve, reject });
			this.#signal();
		});
	}

	/** Drop a stream's pending data (reset / abort). Rejects its in-flight frame. */
	dropStream(streamId: bigint, err: Error): void {
		this.#sendOrders.delete(streamId);
		const waiter = this.#ready.get(streamId);
		if (waiter) {
			this.#ready.delete(streamId);
			waiter.reject(err);
		}
	}

	/** Forget a stream that finished cleanly (its FIN is already queued/sent).
	 *  Frees the per-stream priority entry so it doesn't accumulate. */
	forget(streamId: bigint): void {
		this.#sendOrders.delete(streamId);
	}

	/** Close the scheduler: drop everything still queued and reject pending
	 *  stream frames, then send `final` (the session's close frame) next.
	 *  Resolves once the loop has nothing left to write: `final` was handed to
	 *  the sink, or the sink failed. A write already in flight finishes first. */
	close(err: Error, final?: Uint8Array): Promise<void> {
		if (this.#closed) return this.#done.promise;
		this.#closed = err;
		for (const waiter of this.#ready.values()) waiter.reject(err);
		this.#ready.clear();
		this.#sendOrders.clear();
		// Nothing queued means anything once closed, so the close frame goes next.
		this.#control.length = 0;
		this.#latest.clear();
		this.#datagrams.length = 0;
		if (final) this.#control.push(final);
		this.#signal();
		return this.#done.promise;
	}

	#signal(): void {
		const wake = this.#wake;
		if (wake) {
			this.#wake = undefined;
			wake();
		}
	}

	/** Pick the next stream to service: highest sendOrder, oldest seq to break
	 *  ties (round-robin, since a stream re-arms with a fresh seq each frame). */
	#pickStream(): bigint {
		let bestId: bigint | undefined;
		let bestOrder = Number.NEGATIVE_INFINITY;
		let bestSeq = Number.POSITIVE_INFINITY;
		for (const [id, waiter] of this.#ready) {
			const order = this.#sendOrders.get(id) ?? DEFAULT_SEND_ORDER;
			if (order > bestOrder || (order === bestOrder && waiter.seq < bestSeq)) {
				bestOrder = order;
				bestSeq = waiter.seq;
				bestId = id;
			}
		}
		// Non-null: only called when #ready is non-empty.
		return bestId as bigint;
	}

	async #run(first?: Uint8Array): Promise<void> {
		try {
			if (first) {
				await this.#sink.ready();
				await this.#sink.write(first);
				this.#onActivity();
			}

			while (true) {
				if (
					this.#control.length === 0 &&
					this.#latest.size === 0 &&
					this.#datagrams.length === 0 &&
					this.#ready.size === 0
				) {
					if (this.#closed) return;
					await new Promise<void>((resolve) => {
						this.#wake = resolve;
					});
					continue;
				}

				// Block until the socket can take a write, THEN pick — so a frame
				// that arrives during backpressure is considered before we commit.
				await this.#sink.ready();

				if (this.#control.length > 0) {
					const bytes = this.#control.shift() as Uint8Array;
					await this.#sink.write(bytes);
					this.#onActivity();
					continue;
				}

				const latest = this.#latest.entries().next();
				if (!latest.done) {
					const [key, bytes] = latest.value;
					this.#latest.delete(key);
					await this.#sink.write(bytes);
					this.#onActivity();
					continue;
				}

				if (this.#datagrams.length > 0) {
					const bytes = this.#datagrams.shift() as Uint8Array;
					await this.#sink.write(bytes);
					this.#onActivity();
					continue;
				}

				if (this.#closed) return; // closed with only stream frames left (already rejected)
				if (this.#ready.size === 0) continue;

				const id = this.#pickStream();
				const waiter = this.#ready.get(id) as Waiter;
				this.#ready.delete(id);
				try {
					await this.#sink.write(waiter.bytes);
				} catch (err) {
					// The frame never made it; reject this stream's promise (it's no
					// longer in #ready, so #fail wouldn't) and tear down the rest.
					waiter.reject(err instanceof Error ? err : new Error(String(err)));
					throw err;
				}
				this.#onActivity();
				waiter.resolve();
			}
		} catch (err) {
			this.#fail(err instanceof Error ? err : new Error(String(err)));
		} finally {
			this.#done.resolve();
		}
	}

	#fail(err: Error): void {
		this.#closed ??= err;
		for (const waiter of this.#ready.values()) waiter.reject(err);
		this.#ready.clear();
		this.#control.length = 0;
		this.#latest.clear();
		this.#datagrams.length = 0;
		this.#onFail(err);
	}
}

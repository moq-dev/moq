/** Prioritized send scheduler for a QMux session.
 *
 * A single writer loop owns the socket. Frames are written in priority order:
 * control frames (flow control, resets, pings, close) always preempt stream
 * data, and among stream data the highest `sendOrder` wins (W3C WebTransport
 * convention - larger value is sent first), with round-robin among equal
 * priorities. Prioritization only bites under backpressure; when the sink has
 * room every frame is written as soon as it's queued.
 *
 * The backpressure source is abstracted behind {@link SendSink}: the ponyfill
 * reports its `bufferedAmount` backlog itself, while {@link BacklogSink} counts
 * the backlog of any other writable, such as the native `WebSocketStream`.
 */

/** A write sink over the underlying socket, exposing backpressure. */
export interface SendSink {
	/** Resolve once the sink can accept another write without exceeding its
	 *  high-water mark. Rejects if the socket is closed/errored. */
	ready(): Promise<void>;
	/** Write bytes to the socket, resolving once the write is accepted. Rejects
	 *  if the write fails - the scheduler must observe this rather than resolve
	 *  the frame's promise as if it succeeded. */
	write(bytes: Uint8Array): Promise<void>;
	/** Synchronous backpressure probe: true when the sink can take a write now
	 *  without buffering past its high-water mark. Used to drop datagrams the
	 *  instant the transport starts queuing, rather than letting them pile up
	 *  behind a stream/control backlog and arrive stale. */
	wantsMore(): boolean;
	/** Resolve once every accepted write has reached the socket. Rejects if a
	 *  write failed. */
	flush(): Promise<void>;
	/** Settles when the socket's writable closes; rejects once it fails, even
	 *  after a write it already accepted. */
	readonly closed: Promise<void>;
}

/** Sink over a writable whose `desiredSize` already tracks a byte backlog, as
 *  the `WebSocketStream` ponyfill's does from `bufferedAmount` against its
 *  (resizable) high-water mark. Each write resolves once the socket holds it. */
export class WritableStreamSink implements SendSink {
	#writer: WritableStreamDefaultWriter<Uint8Array>;

	constructor(writable: WritableStream<Uint8Array>) {
		this.#writer = writable.getWriter();
	}

	get closed(): Promise<void> {
		return this.#writer.closed;
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
		const desired = this.#writer.desiredSize;
		return desired !== null && desired > 0;
	}

	flush(): Promise<void> {
		// A resolved write is already in the socket, which flushes it before closing.
		return Promise.resolve();
	}
}

/** Sink over a writable whose own backpressure counts writes rather than bytes,
 *  such as the native `WebSocketStream`: its `desiredSize` reaches 0 while any
 *  one write is in flight, however idle the socket. This hands each write over
 *  at once and counts the bytes whose write has yet to complete as the send
 *  backlog, held under a resizable high-water mark. */
export class BacklogSink implements SendSink {
	#writer: WritableStreamDefaultWriter<Uint8Array>;
	#highWaterMark: number;
	#pending = 0;
	#error?: Error;
	// Settles whenever the backlog shrinks or a write fails, then is replaced.
	#changed = Promise.withResolvers<void>();

	constructor(writable: WritableStream<Uint8Array>, highWaterMark: number) {
		this.#writer = writable.getWriter();
		this.#highWaterMark = highWaterMark;
	}

	get closed(): Promise<void> {
		return this.#writer.closed;
	}

	/** Resize the high-water mark (bytes), waking a write parked behind the old one. */
	setHighWaterMark(bytes: number): void {
		this.#highWaterMark = bytes;
		this.#notify();
	}

	async ready(): Promise<void> {
		for (;;) {
			if (this.#error) throw this.#error;
			if (this.#pending <= this.#highWaterMark) return;
			await this.#changed.promise;
		}
	}

	write(bytes: Uint8Array): Promise<void> {
		if (this.#error) return Promise.reject(this.#error);
		const size = bytes.byteLength;
		this.#pending += size;
		this.#writer.write(bytes).then(
			() => {
				this.#pending -= size;
				this.#notify();
			},
			(err) => {
				this.#error ??= err instanceof Error ? err : new Error(String(err));
				this.#notify();
			},
		);
		return this.ready();
	}

	wantsMore(): boolean {
		return !this.#error && this.#pending <= this.#highWaterMark;
	}

	async flush(): Promise<void> {
		for (;;) {
			if (this.#error) throw this.#error;
			if (this.#pending === 0) return;
			await this.#changed.promise;
		}
	}

	#notify(): void {
		this.#changed.resolve();
		this.#changed = Promise.withResolvers<void>();
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
	/** Encodes the next pending update (MAX_DATA, MAX_STREAMS, MAX_STREAM_DATA), or
	 *  returns undefined once none is owed. Polled after {@link SendScheduler.notify}. */
	pending?: () => Uint8Array | undefined;
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
	#failed = false;

	// Control frames preempt all stream data, FIFO among themselves. Lossless and
	// uncapped, so whatever the peer can trigger here must be bounded by design: a
	// stream's state allows it at most one RESET_STREAM and one STOP_SENDING, and the
	// peer opens streams only as fast as it reads the MAX_STREAMS we send.
	#control: Uint8Array[] = [];

	// Window updates are not queued at all: the session flags them and this polls
	// `#pending` for the newest values when it next writes, so one is owed at most per
	// window however often credit returns.
	#pending: () => Uint8Array | undefined;
	#notified = false;

	// Control frames where only the newest matters (QX_PING requests and
	// responses), keyed by kind. A newer frame replaces one still unsent, so a peer
	// that pings faster than it reads our responses can't grow this lane.
	#latest = new Map<string, Uint8Array>();

	// Datagrams: serviced after control, ahead of stream data. Bounded and lossy
	// (unlike #control) since unreliable datagrams are meant to be droppable -
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
		this.#pending = options?.pending ?? (() => undefined);
		// Start the writer loop. It never rejects - failures are captured by #fail.
		void this.#run(options?.first);
		// A write the sink already accepted can fail while the loop is parked.
		sink.closed.catch((err) => {
			if (this.#closed) return;
			this.#fail(err instanceof Error ? err : new Error(String(err)));
			this.#signal();
		});
	}

	/** Queue a pre-encoded control frame. Preempts all stream data. */
	enqueueControl(bytes: Uint8Array): void {
		if (this.#closed) throw this.#closed;
		this.#control.push(bytes);
		this.#signal();
	}

	/** Flag that a window update is pending; the loop polls for it after the control lane. */
	notify(): void {
		if (this.#closed) return;
		this.#notified = true;
		this.#signal();
	}

	/** Queue a control frame that supersedes any unsent one with the same `key`.
	 *  Serviced after the FIFO control lane, ahead of datagrams and stream data. */
	replaceControl(key: string, bytes: Uint8Array): void {
		if (this.#closed) throw this.#closed;
		this.#latest.set(key, bytes);
		this.#signal();
	}

	/** Queue a datagram frame - best-effort, serviced after control but ahead of
	 *  stream data. Dropped (rather than queued) when:
	 *   - the scheduler is closed;
	 *   - the transport is backpressured - the socket is already queuing stream/
	 *     control data, so a datagram behind it would arrive stale; or
	 *   - the datagram lane is full, bounding a synchronous burst that outruns the
	 *     writer loop even while the transport has room.
	 *  An unreliable datagram is meant to be droppable, so this sheds rather than
	 *  applying backpressure to the caller. */
	enqueueDatagram(bytes: Uint8Array): void {
		if (this.#closed) return;
		if (!this.#sink.wantsMore()) return; // transport is queuing - drop, don't delay
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

	/** Queue a stream-data frame. Resolves once the sink accepts the bytes;
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
	 *  Resolves once the loop has nothing left to write: every accepted write,
	 *  `final` included, reached the socket, or the sink failed. A write already
	 *  in flight finishes first. */
	close(err: Error, final?: Uint8Array): Promise<void> {
		if (this.#closed) return this.#done.promise;
		this.#closed = err;
		for (const waiter of this.#ready.values()) waiter.reject(err);
		this.#ready.clear();
		this.#sendOrders.clear();
		// Nothing queued means anything once closed, so the close frame goes next.
		this.#control.length = 0;
		this.#notified = false;
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
					!this.#notified &&
					this.#latest.size === 0 &&
					this.#datagrams.length === 0 &&
					this.#ready.size === 0
				) {
					if (this.#closed) break;
					await new Promise<void>((resolve) => {
						this.#wake = resolve;
					});
					continue;
				}

				// Block until the socket can take a write, THEN pick - so a frame
				// that arrives during backpressure is considered before we commit.
				await this.#sink.ready();

				if (this.#control.length > 0) {
					const bytes = this.#control.shift() as Uint8Array;
					await this.#sink.write(bytes);
					this.#onActivity();
					continue;
				}

				if (this.#notified) {
					const bytes = this.#pending();
					if (bytes) {
						await this.#sink.write(bytes);
						this.#onActivity();
					} else {
						this.#notified = false;
					}
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

				if (this.#closed) break; // closed with only stream frames left (already rejected)
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
			// Closed: the close frame counts as sent only once the socket holds it.
			await this.#sink.flush();
		} catch (err) {
			this.#fail(err instanceof Error ? err : new Error(String(err)));
		} finally {
			this.#done.resolve();
		}
	}

	#fail(err: Error): void {
		if (this.#failed) return;
		this.#failed = true;
		this.#closed ??= err;
		for (const waiter of this.#ready.values()) waiter.reject(err);
		this.#ready.clear();
		this.#control.length = 0;
		this.#notified = false;
		this.#latest.clear();
		this.#datagrams.length = 0;
		this.#onFail(err);
	}
}

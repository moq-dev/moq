/** The receive half of a multiplexed stream.
 *
 * Buffers incoming STREAM data and hands it to the application's
 * `ReadableStream` *on demand* (pull-driven). `onConsume` fires only when bytes
 * are actually delivered to the reader — never on mere receipt — so flow-control
 * credit (MAX_STREAM_DATA) tracks the application's read rate. Combined with the
 * sender claiming credit before sending, this bounds how far the peer can run
 * ahead of a slow reader: it can buffer at most one receive window of undelivered
 * data before its credit is exhausted.
 *
 * A zero high-water mark prevents the `ReadableStream` from prefetching even one
 * chunk before the application issues a read; all undelivered data stays in the
 * explicitly flow-controlled queue below.
 */
export class RecvStream {
	#queue: Uint8Array[] = [];
	// Bytes handed to the reader so far: the stream offset where #queue starts.
	#delivered = 0n;
	#fin = false;
	#error?: Error;
	#wake?: () => void;
	#terminal = false;
	#onTerminal: () => void;

	/** The application-facing readable. */
	readonly readable: ReadableStream<Uint8Array>;

	/**
	 * @param onConsume Invoked with each chunk's byte length as it is delivered
	 *   to the reader. Drives MAX_STREAM_DATA.
	 * @param onCancel Invoked with the number of discarded buffered bytes, and the reason the
	 *   application passed to `cancel()`, when the application cancels the readable
	 *   (→ STOP_SENDING, whose code comes from that reason).
	 * @param onTerminal Invoked once when FIN, RESET_STREAM, or local cancellation
	 *   makes the receive side terminal.
	 */
	constructor(
		onConsume: (bytes: number) => void,
		onCancel: (discarded: number, reason: unknown) => void,
		onTerminal: () => void = () => {},
	) {
		this.#onTerminal = onTerminal;
		this.readable = new ReadableStream<Uint8Array>(
			{
				pull: async (controller) => {
					while (this.#queue.length === 0) {
						if (this.#error) {
							controller.error(this.#error);
							return;
						}
						if (this.#fin) {
							controller.close();
							return;
						}
						await new Promise<void>((resolve) => {
							this.#wake = resolve;
						});
					}
					const chunk = this.#queue.shift() as Uint8Array;
					this.#delivered += BigInt(chunk.byteLength);
					controller.enqueue(chunk);
					// Credit must track *delivery*, not receipt — do NOT move onConsume
					// into push(). With a zero high-water mark, pull only runs for an
					// application read, so even the first chunk cannot be prefetched into
					// an unaccepted stream's hidden ReadableStream queue.
					onConsume(chunk.byteLength);
				},
				cancel: (reason: unknown) => {
					this.#error ??= new Error("stream cancelled");
					onCancel(this.#discard(), reason);
					this.#notifyTerminal();
					this.#signal();
				},
			},
			{ highWaterMark: 0 },
		);
	}

	/** Buffer a received chunk for delivery. Ignored after FIN/error. */
	push(chunk: Uint8Array): boolean {
		if (this.#fin || this.#error) return false;
		this.#queue.push(chunk);
		this.#signal();
		return true;
	}

	/** Mark end-of-stream; the readable closes once buffered data drains. */
	finish(): void {
		this.#fin = true;
		this.#notifyTerminal();
		this.#signal();
	}

	/** Abort the readable because the peer reset its sending side, once the reader
	 *  has the first `reliableSize` bytes of the stream (RESET_STREAM_AT). Discards
	 *  the buffered bytes past that prefix and returns how many. */
	reset(err: Error, reliableSize = 0n): number {
		let discarded = 0;
		if (!this.#error) {
			this.#error = err;
			let keep = reliableSize - this.#delivered;
			const queue: Uint8Array[] = [];
			for (const chunk of this.#queue) {
				const size = BigInt(chunk.byteLength);
				if (keep >= size) {
					queue.push(chunk);
					keep -= size;
				} else {
					const kept = keep > 0n ? Number(keep) : 0;
					if (kept > 0) queue.push(chunk.subarray(0, kept));
					discarded += chunk.byteLength - kept;
					keep = 0n;
				}
			}
			// pull() delivers what is left before surfacing the error.
			this.#queue = queue;
			this.#signal();
		}
		this.#notifyTerminal();
		return discarded;
	}

	/** Abort the readable, discarding undelivered buffered data. Also discards a
	 *  reliable prefix a reset left queued; the first error is kept. */
	error(err: Error): number {
		this.#error ??= err;
		const discarded = this.#discard();
		this.#signal();
		return discarded;
	}

	#discard(): number {
		let bytes = 0;
		for (const chunk of this.#queue) bytes += chunk.byteLength;
		this.#queue = [];
		return bytes;
	}

	#notifyTerminal(): void {
		if (this.#terminal) return;
		this.#terminal = true;
		this.#onTerminal();
	}

	#signal(): void {
		const wake = this.#wake;
		if (wake) {
			this.#wake = undefined;
			wake();
		}
	}
}

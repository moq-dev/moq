import { VarInt } from "./varint.ts";

/** Decide the new receive-window limit to advertise (MAX_DATA / MAX_STREAM_DATA).
 *
 * Replenishes only when the remaining advertised window has dropped to at most
 * half, and always sets the new limit relative to bytes the application has
 * actually **consumed** (`consumed` is cumulative) - never relative to bytes
 * received. That keeps the peer's send credit bounded to one `window` ahead of
 * the reader: a stalled reader stops advancing `consumed`, so the limit stops
 * moving and the peer's credit drains.
 *
 * Returns the new limit, or `null` if no update is warranted.
 */
export function replenishWindow(consumed: bigint, currentMax: bigint, window: bigint): bigint | null {
	if (window === 0n) return null;
	if (currentMax - consumed <= window / 2n) return consumed + window;
	return null;
}

/** Credit the peer grants us to send, as MAX_DATA, MAX_STREAM_DATA, or MAX_STREAMS.
 *
 * Callers can synchronously try to claim credit, or await until credit becomes
 * available. Calling `close()` causes pending and future `claim()` calls to reject.
 */
export class Credit {
	#used: bigint;
	#max: bigint;
	#closed = false;
	#closeReason?: Error;
	// A Set, so an aborted claim unlinks itself in O(1).
	#waiters = new Set<{ resolve: () => void; reject: (err: Error) => void }>();

	constructor(max: bigint) {
		this.#used = 0n;
		this.#max = max;
	}

	/** Try to claim up to `limit` units. Returns amount claimed (0n if none
	 *  available or closed). */
	tryClaim(limit: bigint): bigint {
		if (limit === 0n || this.#closed) return 0n;
		const available = this.#max - this.#used;
		if (available <= 0n) return 0n;
		const claimed = limit < available ? limit : available;
		this.#used += claimed;
		return claimed;
	}

	/** Claim up to `limit` units, waiting until credit is available.
	 *  Rejects if the credit has been closed, or with `signal`'s reason once it
	 *  aborts. Returns 0n for zero-limit requests. */
	async claim(limit: bigint, signal?: AbortSignal): Promise<bigint> {
		if (limit === 0n) return 0n;

		while (true) {
			if (this.#closed) throw this.#closeReason ?? new Error("closed");
			signal?.throwIfAborted();

			const claimed = this.tryClaim(limit);
			if (claimed > 0n) return claimed;

			await this.#wait(signal);
		}
	}

	/** Wait until credit may be available, without claiming it. Rejects like {@link claim}. */
	async wait(signal?: AbortSignal): Promise<void> {
		if (this.#closed) throw this.#closeReason ?? new Error("closed");
		signal?.throwIfAborted();
		if (this.#max > this.#used) return;
		await this.#wait(signal);
		if (this.#closed) throw this.#closeReason ?? new Error("closed");
		// The signal may have aborted between the wake and this resuming.
		signal?.throwIfAborted();
	}

	#wait(signal?: AbortSignal): Promise<void> {
		return new Promise<void>((resolve, reject) => {
			// Remove the listener on wake or close too, so a long-lived signal
			// doesn't collect one per parked claim.
			const waiter = {
				resolve: () => {
					signal?.removeEventListener("abort", abort);
					resolve();
				},
				reject: (err: Error) => {
					signal?.removeEventListener("abort", abort);
					reject(err);
				},
			};
			const abort = () => {
				this.#waiters.delete(waiter);
				reject(signal?.reason);
			};
			signal?.addEventListener("abort", abort, { once: true });
			this.#waiters.add(waiter);
		});
	}

	/** Increase the max. Returns false if new_max < current max. */
	increaseMax(newMax: bigint): boolean {
		if (newMax < this.#max) return false;
		if (newMax === this.#max) return true;
		this.#max = newMax;
		this.#wake();
		return true;
	}

	/** Close the credit, rejecting all pending and future `claim()` calls.
	 *  `reason` (if provided) becomes the rejection error; the first reason wins. */
	close(reason?: Error): void {
		this.#closed = true;
		this.#closeReason ??= reason ?? new Error("closed");
		const waiters = this.#waiters;
		this.#waiters = new Set();
		for (const { reject } of waiters) reject(this.#closeReason);
	}

	/** Get current available credit (max - used). */
	get available(): bigint {
		const avail = this.#max - this.#used;
		return avail > 0n ? avail : 0n;
	}

	#wake(): void {
		const waiters = this.#waiters;
		this.#waiters = new Set();
		for (const { resolve } of waiters) resolve();
	}
}

/** A receive window we advertise with MAX_DATA or MAX_STREAM_DATA.
 *
 * `received` is how far the peer has spent it, `consumed` how much the
 * application has read or discarded. Releasing credit marks an update pending
 * rather than sending one: the scheduler takes it with {@link takePending} when
 * it next writes, so however many releases happen first, the peer gets one frame
 * carrying the newest limit. The peer is held to the limit it was last sent,
 * not one still waiting behind a busy socket.
 */
export class Window {
	readonly #size: bigint;
	// The limit owed to the peer, which `advertised` catches up to once taken.
	#max: bigint;
	#advertised: bigint;
	#received = 0n;
	#consumed = 0n;
	#pending = false;
	#onPending: () => void;

	/** `onPending` runs when an update becomes pending, so the scheduler can wake. */
	constructor(size: bigint, onPending: () => void) {
		this.#size = size;
		this.#max = size;
		this.#advertised = size;
		this.#onPending = onPending;
	}

	/** The newest limit, which the next update carries. */
	get max(): bigint {
		return this.#max;
	}

	/** How far the peer has spent the window. */
	get received(): bigint {
		return this.#received;
	}

	/** Whether the peer may spend up to `end`. */
	allows(end: bigint): boolean {
		return end <= this.#advertised;
	}

	/** Spend the window up to `end`, which the caller has checked with {@link allows}. */
	receiveTo(end: bigint): void {
		if (end > this.#advertised) throw new Error("flow control error");
		if (end > this.#received) this.#received = end;
	}

	/** Return `bytes` of credit, marking an update pending once half the window is spent. */
	release(bytes: bigint): void {
		if (bytes === 0n) return;
		this.#consumed += bytes;
		let next = replenishWindow(this.#consumed, this.#max, this.#size);
		if (next === null) return;
		// A varint can't carry more; a peer that reached it has nothing left to send.
		if (next > VarInt.MAX) next = VarInt.MAX;
		if (next <= this.#max) return;
		this.#max = next;
		if (this.#pending) return;
		this.#pending = true;
		this.#onPending();
	}

	/** Whether an update is owed, clearing it. The caller sends {@link max}, which
	 *  becomes the limit the peer is held to. */
	takePending(): boolean {
		const pending = this.#pending;
		this.#pending = false;
		if (pending) this.#advertised = this.#max;
		return pending;
	}
}

/** The streams a peer may open in one direction, which we advertise with MAX_STREAMS.
 *
 * A stream's slot returns once the stream is closed; the limit rises once the
 * returned slots would let the peer open past it, so a peer at its limit gets
 * each slot back promptly. Like {@link Window}, a rise marks an update pending
 * for the scheduler, and the peer is held to the limit it was last sent.
 */
export class StreamLimit {
	// The limit owed to the peer, which `advertised` catches up to once taken.
	#max: bigint;
	#advertised: bigint;
	// One past the highest stream index the peer opened, which opens every lower one.
	#opened = 0n;
	// Closed streams whose slots have yet to be advertised.
	#closed = 0n;
	#pending = false;
	#onPending: () => void;

	constructor(max: bigint, onPending: () => void) {
		this.#max = max;
		this.#advertised = max;
		this.#onPending = onPending;
	}

	/** The newest limit, which the next update carries. */
	get max(): bigint {
		return this.#max;
	}

	/** Open stream `index` and every lower one, or return false if that passes the limit. */
	open(index: bigint): boolean {
		if (index >= this.#advertised) return false;
		if (index >= this.#opened) {
			this.#opened = index + 1n;
			// The peer moved closer to the limit, so slots held back may be due now.
			this.#raise();
		}
		return true;
	}

	/** Return a closed stream's slot. */
	close(): void {
		this.#closed += 1n;
		this.#raise();
	}

	/** Advertise the returned slots once they would let the peer open past the limit. */
	#raise(): void {
		if (this.#closed === 0n || this.#opened + 2n * this.#closed <= this.#max) return;
		this.#max += this.#closed;
		this.#closed = 0n;
		if (this.#pending) return;
		this.#pending = true;
		this.#onPending();
	}

	/** Whether an update is owed, clearing it. The caller sends {@link max}, which
	 *  becomes the limit the peer is held to. */
	takePending(): boolean {
		const pending = this.#pending;
		this.#pending = false;
		if (pending) this.#advertised = this.#max;
		return pending;
	}
}

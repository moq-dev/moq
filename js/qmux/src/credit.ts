/** Decide the new receive-window limit to advertise (MAX_DATA / MAX_STREAM_DATA).
 *
 * Replenishes only when the remaining advertised window has dropped to at most
 * half, and always sets the new limit relative to bytes the application has
 * actually **consumed** (`consumed` is cumulative) — never relative to bytes
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

/** Tracks used/max credit for flow control.
 *
 * Mirrors the Rust `Credit` struct. Callers can synchronously try to claim
 * credit, or await until credit becomes available. Calling `close()` causes
 * pending and future `claim()` calls to reject.
 */
export class Credit {
	#used: bigint;
	#max: bigint;
	#released = 0n;
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

			await new Promise<void>((resolve, reject) => {
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
	}

	/** Return previously claimed credit (for rollback). */
	release(amount: bigint): void {
		this.#used = this.#used > amount ? this.#used - amount : 0n;
		this.#wake();
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

	/** Set used to max(used, value). Returns false if value > max (flow control violation). */
	receiveUpTo(value: bigint): boolean {
		if (value > this.#max) return false;
		if (value > this.#used) this.#used = value;
		return true;
	}

	/** Report that `len` units have been consumed.
	 *  Returns the new max if a window update should be sent, or null otherwise. */
	consume(len: bigint): bigint | null {
		this.#released += len;

		// Send a window update when: used + 2*released > max
		if (this.#used + 2n * this.#released > this.#max) {
			const newMax = this.#max + this.#released;
			this.#max = newMax;
			this.#released = 0n;
			this.#wake();
			return newMax;
		}
		return null;
	}

	/** Get current available credit (max - used). */
	get available(): bigint {
		const avail = this.#max - this.#used;
		return avail > 0n ? avail : 0n;
	}

	/** Get the current max value. */
	get max(): bigint {
		return this.#max;
	}

	/** Get the current used value. */
	get used(): bigint {
		return this.#used;
	}

	#wake(): void {
		const waiters = this.#waiters;
		this.#waiters = new Set();
		for (const { resolve } of waiters) resolve();
	}
}

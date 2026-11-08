import { Credit } from "./credit.ts";

/** A send half's state, from RFC 9000 §3.1.
 *
 * QMux acknowledges whatever reaches the socket, so "Data Recvd" is a FIN on the
 * socket and a RESET_STREAM is received as soon as it is sent. */
export type SendState = "ready" | "dataSent" | "dataRecvd" | "resetSent";

/** The send half of a stream: its state, the peer's MAX_STREAM_DATA, and its offset.
 *
 * Bytes count as sent once they take credit, before the scheduler writes them, as
 * Rust's send buffer counts them. So the final size a FIN or RESET_STREAM carries
 * covers every byte the peer may yet see, including a frame mid-write or still
 * queued. */
export class SendHalf {
	#state: SendState = "ready";
	#offset = 0n;
	// Aborted by a reset, ending a write parked on either credit.
	#reset = new AbortController();
	/** The peer's MAX_STREAM_DATA for this stream. */
	readonly credit: Credit;

	constructor(max: bigint) {
		this.credit = new Credit(max);
	}

	/** The current state. */
	get state(): SendState {
		return this.#state;
	}

	/** The bytes sent so far, which is where the next frame starts and the final size. */
	get offset(): bigint {
		return this.#offset;
	}

	/** Whether the half is done on the wire: its FIN reached the socket, or it was reset. */
	get terminal(): boolean {
		return this.#state === "dataRecvd" || this.#state === "resetSent";
	}

	/** Take up to `desired` bytes of both stream and connection credit, waiting for
	 *  the peer to grant some, and count them as sent. Rejects once the half is reset,
	 *  the session closes, or `signal` aborts. */
	async claim(desired: bigint, conn: Credit, signal: AbortSignal): Promise<bigint> {
		const ended = AbortSignal.any([signal, this.#reset.signal]);
		for (;;) {
			await this.credit.wait(ended);
			await conn.wait(ended);
			ended.throwIfAborted();
			// Another stream may have spent the connection credit while this waited.
			const stream = this.credit.available;
			const shared = conn.available;
			let n = desired < stream ? desired : stream;
			if (shared < n) n = shared;
			if (n === 0n) continue;
			this.credit.tryClaim(n);
			conn.tryClaim(n);
			this.#offset += n;
			return n;
		}
	}

	/** Throw the reset's reason once the half is reset, as a write that claimed credit
	 *  just before a STOP_SENDING must, rather than queue a frame after the reset. */
	throwIfReset(): void {
		this.#reset.signal.throwIfAborted();
	}

	/** Queue the FIN, returning the final size. */
	finish(): bigint {
		if (this.#state !== "ready") throw new Error(`cannot finish a send half in ${this.#state}`);
		this.#state = "dataSent";
		return this.#offset;
	}

	/** The FIN reached the socket. */
	finished(): void {
		if (this.#state === "dataSent") this.#state = "dataRecvd";
	}

	/** Reset the half, returning the final size for RESET_STREAM, or undefined if it
	 *  already ended. Writes parked on its credit reject with `reason`. */
	reset(reason: Error): bigint | undefined {
		if (this.terminal) return undefined;
		this.#state = "resetSent";
		this.#reset.abort(reason);
		return this.#offset;
	}
}

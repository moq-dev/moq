import type { Window } from "./credit.ts";
import { Queue } from "./queue.ts";

/** A receive half's phase, each carrying its own data: the RFC 9000 §3.2 states,
 *  plus `stopped` once the application stops reading.
 *
 * The transport is ordered, so a FIN means every byte has arrived: "Size Known"
 * and "Data Recvd" are one state, `sizeKnown`. A stopped half buffers nothing and
 * only tracks whether the peer has sent its final size. */
type Phase =
	| { state: "recv"; queue: Queue<Uint8Array> }
	| { state: "sizeKnown"; queue: Queue<Uint8Array> }
	| { state: "dataRead" }
	| { state: "resetRecvd"; queue: Queue<Uint8Array>; error: Error }
	| { state: "resetRead"; error: Error }
	| { state: "stopped"; error: Error; sizeKnown: boolean };

/** A receive half's state. */
export type RecvState = Phase["state"];

/** What a receive half charges and tells the session about. */
export interface RecvStreamOptions {
	/** The connection window (MAX_DATA), which every byte charges until it is read or discarded. */
	conn: Window;
	/** This stream's window (MAX_STREAM_DATA), replenished as the application reads. */
	window: Window;
	/** Sends STOP_SENDING with the application's cancel reason. */
	stop: (reason: unknown) => void;
}

/** The receive half of a multiplexed stream: its state, flow control, and buffer.
 *
 * Buffers incoming STREAM data and hands it to the application's
 * `ReadableStream` *on demand* (pull-driven). Credit returns only as bytes are
 * delivered to the reader or discarded, never on mere receipt, so the peer can
 * buffer at most one receive window of undelivered data.
 *
 * Every byte the peer sends charges the connection window, including bytes that
 * arrive after the application stopped reading and a RESET_STREAM's final-size
 * gap, and each returns once it is delivered or discarded. That accounting
 * happens only inside this class's transitions.
 *
 * A zero high-water mark prevents the `ReadableStream` from prefetching even one
 * chunk before the application issues a read; all undelivered data stays in the
 * explicitly flow-controlled queue below.
 */
export class RecvStream {
	#phase: Phase = { state: "recv", queue: new Queue() };
	// The end of the bytes received, and the final size once known.
	#end = 0n;
	// Bytes handed to the reader so far: the stream offset where the queue starts.
	#delivered = 0n;
	#wake?: () => void;
	#conn: Window;
	#window: Window;
	#sendStop: (reason: unknown) => void;

	/** The application-facing readable. */
	readonly readable: ReadableStream<Uint8Array>;

	constructor(options: RecvStreamOptions) {
		this.#conn = options.conn;
		this.#window = options.window;
		this.#sendStop = options.stop;
		this.readable = new ReadableStream<Uint8Array>(
			{
				pull: async (controller) => {
					let phase = this.#phase;
					while (!("queue" in phase) || phase.queue.length === 0) {
						switch (phase.state) {
							case "recv":
								await new Promise<void>((resolve) => {
									this.#wake = resolve;
								});
								phase = this.#phase;
								continue;
							case "sizeKnown":
								this.#phase = { state: "dataRead" };
								controller.close();
								return;
							case "dataRead":
								controller.close();
								return;
							case "resetRecvd":
								this.#phase = { state: "resetRead", error: phase.error };
								controller.error(phase.error);
								return;
							case "resetRead":
							case "stopped":
								controller.error(phase.error);
								return;
						}
					}
					const chunk = phase.queue.shift() as Uint8Array;
					const bytes = BigInt(chunk.byteLength);
					this.#delivered += bytes;
					controller.enqueue(chunk);
					// Credit must track *delivery*, not receipt. With a zero high-water mark,
					// pull only runs for an application read, so even the first chunk cannot be
					// prefetched into an unaccepted stream's hidden ReadableStream queue.
					this.#conn.release(bytes);
					if (phase.state === "recv") this.#window.release(bytes);
				},
				cancel: (reason: unknown) => {
					// Only a peer still sending has anything to stop.
					if (this.#phase.state === "recv") options.stop(reason);
					this.#stop(new Error("stream cancelled"));
				},
			},
			{ highWaterMark: 0 },
		);
	}

	/** The current state. */
	get state(): RecvState {
		return this.#phase.state;
	}

	/** Whether the final size is known, from a FIN or RESET_STREAM, so no more bytes can arrive. */
	get sizeKnown(): boolean {
		const phase = this.#phase;
		if (phase.state === "stopped") return phase.sizeKnown;
		return phase.state !== "recv";
	}

	/** Whether MAX_STREAM_DATA still matters: the peer is sending and we still want its bytes. */
	get wantsCredit(): boolean {
		return this.#phase.state === "recv";
	}

	/** The stream's window, for the MAX_STREAM_DATA it owes. */
	get window(): Window {
		return this.#window;
	}

	/** The end of the bytes received, and the final size once known. */
	get end(): bigint {
		return this.#end;
	}

	/** Receive a STREAM frame, charging both windows. Throws on a frame that doesn't
	 *  continue the stream or that exceeds a window. A frame after the final size
	 *  is known is dropped. */
	data(offset: bigint | undefined, chunk: Uint8Array, fin: boolean): void {
		const phase = this.#phase;
		if (phase.state !== "recv" && (phase.state !== "stopped" || phase.sizeKnown)) return;
		// The transport is ordered, so a frame must start exactly where the last one
		// ended (draft-02 §4.1): there is nothing to reassemble, and anything else
		// would deliver bytes out of order or twice. An absent offset comes from a
		// sender released before offsets were encoded, whose frames are contiguous.
		if (offset !== undefined && offset !== this.#end) {
			throw new Error(`STREAM offset ${offset} does not continue the stream at ${this.#end}`);
		}
		const bytes = BigInt(chunk.byteLength);
		const end = this.#end + bytes;
		const total = this.#conn.received + bytes;
		if (!this.#window.allows(end) || !this.#conn.allows(total)) throw new Error("flow control error");
		this.#window.receiveTo(end);
		this.#conn.receiveTo(total);
		this.#end = end;

		if (phase.state === "stopped") {
			// A stopped stream frees the bytes as they land.
			this.#conn.release(bytes);
			if (fin) this.#phase = { state: "stopped", error: phase.error, sizeKnown: true };
		} else {
			if (bytes > 0n) phase.queue.push(chunk);
			if (fin) this.#phase = { state: "sizeKnown", queue: phase.queue };
		}
		this.#signal();
	}

	/** Receive a RESET_STREAM, or a RESET_STREAM_AT whose reader still gets the first
	 *  `reliableSize` bytes. Charges the final-size gap, which never occupies memory, and
	 *  frees it with the bytes past the reliable prefix. A reset after the final size
	 *  is known is dropped.
	 *
	 * Drafts through -02 were emitted by implementations that used an incorrect zero
	 * final size, so a final size below the bytes received counts as those bytes. */
	reset(err: Error, finalSize: bigint, reliableSize = 0n): void {
		const phase = this.#phase;
		if (phase.state !== "recv" && (phase.state !== "stopped" || phase.sizeKnown)) return;
		// The transport is ordered, so every byte the peer sent arrived before this
		// frame. A reliable prefix longer than that promises bytes that will never come.
		if (reliableSize > this.#end) throw new Error("RESET_STREAM_AT reliable_size exceeds the bytes received");
		// TODO(qmux-03): reject finalSize < end with FINAL_SIZE_ERROR instead.
		const size = finalSize > this.#end ? finalSize : this.#end;
		const gap = size - this.#end;
		const total = this.#conn.received + gap;
		if (!this.#window.allows(size) || !this.#conn.allows(total)) throw new Error("flow control error");
		this.#window.receiveTo(size);
		this.#conn.receiveTo(total);
		this.#end = size;

		if (phase.state === "stopped") {
			// Nothing is buffered, so only the gap is freed.
			this.#phase = { state: "stopped", error: phase.error, sizeKnown: true };
			this.#conn.release(gap);
			this.#signal();
			return;
		}

		// Keep the reliable prefix the reader hasn't had yet; pull() delivers it before
		// surfacing the error.
		let keep = reliableSize - this.#delivered;
		let discarded = 0n;
		const queue = new Queue<Uint8Array>();
		for (const chunk of phase.queue) {
			const bytes = BigInt(chunk.byteLength);
			if (keep >= bytes) {
				queue.push(chunk);
				keep -= bytes;
			} else {
				const kept = keep > 0n ? keep : 0n;
				if (kept > 0n) queue.push(chunk.subarray(0, Number(kept)));
				discarded += bytes - kept;
				keep = 0n;
			}
		}
		this.#phase = { state: "resetRecvd", queue, error: err };
		this.#conn.release(gap + discarded);
		this.#signal();
	}

	/** Stop reading without telling the peer, as for a closing session: discards the
	 *  buffer, frees its credit, and errors the reader. */
	refuse(err: Error): void {
		this.#stop(err);
	}

	/** Stop reading as {@link refuse} does, and send STOP_SENDING to a peer still
	 *  sending, as for an incoming stream nobody will accept. Without it, a peer
	 *  parked on this stream's window would never finish it. */
	stop(err: Error): void {
		if (this.#phase.state === "recv") this.#sendStop(err);
		this.#stop(err);
	}

	#stop(err: Error): void {
		const phase = this.#phase;
		// A read-out or already stopped half has nothing left to discard.
		if (!("queue" in phase)) return;
		let bytes = 0n;
		for (const chunk of phase.queue) bytes += BigInt(chunk.byteLength);
		// A reset's error already explains the end to the reader.
		const error = phase.state === "resetRecvd" ? phase.error : err;
		this.#phase = { state: "stopped", error, sizeKnown: phase.state !== "recv" };
		this.#conn.release(bytes);
		this.#signal();
	}

	#signal(): void {
		const wake = this.#wake;
		if (wake) {
			this.#wake = undefined;
			wake();
		}
	}
}

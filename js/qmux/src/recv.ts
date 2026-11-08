import type { Window } from "./credit.ts";

/** A receive half's state, from RFC 9000 §3.2.
 *
 * The transport is ordered, so a FIN means every byte has arrived: "Size Known"
 * and "Data Recvd" are one state, `sizeKnown`. */
export type RecvState = "recv" | "sizeKnown" | "dataRead" | "resetRecvd" | "resetRead";

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
	#queue: Uint8Array[] = [];
	#state: RecvState = "recv";
	// The end of the bytes received, and the final size once known.
	#end = 0n;
	// Bytes handed to the reader so far: the stream offset where #queue starts.
	#delivered = 0n;
	// The application stopped reading, so arriving bytes are discarded.
	#stopped = false;
	#error?: Error;
	#wake?: () => void;
	#conn: Window;
	#window: Window;

	/** The application-facing readable. */
	readonly readable: ReadableStream<Uint8Array>;

	constructor(options: RecvStreamOptions) {
		this.#conn = options.conn;
		this.#window = options.window;
		this.readable = new ReadableStream<Uint8Array>(
			{
				pull: async (controller) => {
					while (this.#queue.length === 0) {
						if (this.#error) {
							if (this.#state === "resetRecvd") this.#state = "resetRead";
							controller.error(this.#error);
							return;
						}
						if (this.#state === "sizeKnown") {
							this.#state = "dataRead";
							controller.close();
							return;
						}
						await new Promise<void>((resolve) => {
							this.#wake = resolve;
						});
					}
					const chunk = this.#queue.shift() as Uint8Array;
					const bytes = BigInt(chunk.byteLength);
					this.#delivered += bytes;
					controller.enqueue(chunk);
					// Credit must track *delivery*, not receipt. With a zero high-water mark,
					// pull only runs for an application read, so even the first chunk cannot be
					// prefetched into an unaccepted stream's hidden ReadableStream queue.
					this.#conn.release(bytes);
					if (this.#state === "recv") this.#window.release(bytes);
				},
				cancel: (reason: unknown) => {
					// Only a peer still sending has anything to stop.
					if (this.#state === "recv" && !this.#stopped) options.stop(reason);
					this.#stop(new Error("stream cancelled"));
				},
			},
			{ highWaterMark: 0 },
		);
	}

	/** The current state. */
	get state(): RecvState {
		return this.#state;
	}

	/** Whether the final size is known, from a FIN or RESET_STREAM, so no more bytes can arrive. */
	get sizeKnown(): boolean {
		return this.#state !== "recv";
	}

	/** Whether MAX_STREAM_DATA still matters: the peer is sending and we still want its bytes. */
	get wantsCredit(): boolean {
		return this.#state === "recv" && !this.#stopped;
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
		if (this.sizeKnown) return;
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

		if (bytes > 0n) {
			// A stopped stream frees the bytes as they land.
			if (this.#stopped) this.#conn.release(bytes);
			else this.#queue.push(chunk);
		}
		if (fin) this.#state = "sizeKnown";
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
		if (this.sizeKnown) return;
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
		this.#state = "resetRecvd";
		this.#error ??= err;

		// Keep the reliable prefix the reader hasn't had yet; pull() delivers it before
		// surfacing the error.
		let keep = reliableSize - this.#delivered;
		let discarded = 0n;
		const queue: Uint8Array[] = [];
		for (const chunk of this.#queue) {
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
		this.#queue = queue;
		this.#conn.release(gap + discarded);
		this.#signal();
	}

	/** Stop reading without telling the peer, as for an incoming stream nobody accepted
	 *  or a closing session: discards the buffer, frees its credit, and errors the reader. */
	refuse(err: Error): void {
		this.#stop(err);
	}

	#stop(err: Error): void {
		this.#stopped = true;
		this.#error ??= err;
		let bytes = 0n;
		for (const chunk of this.#queue) bytes += BigInt(chunk.byteLength);
		this.#queue = [];
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

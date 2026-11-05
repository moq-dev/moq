/** Compatibility helpers for browser WebTransport datagram stream shape changes. */
import type { SendStream } from "./stream.ts";

/** The current standards-track datagram send stream, plus Chrome's legacy writable property. */
interface DatagramDuplex {
	readonly readable?: ReadableStream<Uint8Array>;
	readonly writable?: WritableStream<Uint8Array>;
	readonly maxDatagramSize?: number;
	createWritable?: (options: { sendOrder: number }) => SendStream;
}

function datagrams(quic: WebTransport): DatagramDuplex | undefined {
	return (quic as unknown as { datagrams?: DatagramDuplex }).datagrams;
}

/** The max outgoing datagram payload size, or 0 when this transport cannot send datagrams. */
export function maxDatagramSize(quic: WebTransport): number {
	const size = datagrams(quic)?.maxDatagramSize;
	return typeof size === "number" && size > 0 ? size : 0;
}

/** Whether this transport exposes a receive channel for datagrams. */
export function canReceiveDatagrams(quic: WebTransport): boolean {
	return datagrams(quic)?.readable !== undefined;
}

/** Get a datagram reader, or undefined when this browser/transport does not expose one. */
export function datagramReader(quic: WebTransport): ReadableStreamDefaultReader<Uint8Array> | undefined {
	const readable = datagrams(quic)?.readable;
	if (!readable) {
		console.warn("datagram receive disabled: WebTransport datagrams.readable is unavailable");
		return undefined;
	}

	try {
		return readable.getReader();
	} catch (err: unknown) {
		console.warn("datagram receive disabled: failed to open WebTransport datagram reader", err);
		return undefined;
	}
}

/** One subscription's datagram send queue. */
export interface Sender {
	writer: WritableStreamDefaultWriter<Uint8Array>;
	setPriority(order: number): void;
	close(): Promise<void>;
}

/** Opens independent prioritized queues on current transports, sharing the legacy writer otherwise. */
export function datagramWriter(quic: WebTransport):
	| {
			open(order: number): Sender;
			close(): void;
	  }
	| undefined {
	const stream = datagrams(quic);
	if (!stream || maxDatagramSize(quic) === 0) return undefined;
	const createWritable = stream.createWritable;
	if (typeof createWritable === "function") {
		return {
			open(order) {
				const writable = createWritable.call(stream, { sendOrder: order });
				const writer = writable.getWriter();
				return {
					writer,
					setPriority(order) {
						writable.sendOrder = order;
					},
					async close() {
						try {
							await writer.close();
						} finally {
							writer.releaseLock();
						}
					},
				};
			},
			// Each request owns and closes its own current-API queue.
			close() {},
		};
	}
	if (!stream.writable) return undefined;
	const writer = stream.writable.getWriter();
	return {
		// The legacy API has one session queue and no per-subscription send order.
		open: () => ({ writer, setPriority() {}, async close() {} }),
		close: () => writer.releaseLock(),
	};
}

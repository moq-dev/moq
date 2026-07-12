/** Compatibility helpers for browser WebTransport datagram stream shape changes. */

/** The current standards-track datagram send stream, plus Chrome's legacy writable property. */
interface DatagramDuplex {
	readonly readable?: ReadableStream<Uint8Array>;
	readonly writable?: WritableStream<Uint8Array>;
	readonly maxDatagramSize?: number;
	createWritable?: () => WritableStream<Uint8Array>;
}

function datagrams(quic: WebTransport): DatagramDuplex | undefined {
	return (quic as unknown as { datagrams?: DatagramDuplex }).datagrams;
}

/** The max outgoing datagram payload size, or 0 when this transport cannot send datagrams. */
export function maxDatagramSize(quic: WebTransport): number {
	const size = datagrams(quic)?.maxDatagramSize;
	return typeof size === "number" && size > 0 ? size : 0;
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

/** Get a datagram writer, or undefined when this browser/transport does not expose one. */
export function datagramWriter(quic: WebTransport): WritableStreamDefaultWriter<Uint8Array> | undefined {
	const stream = datagrams(quic);
	if (!stream || maxDatagramSize(quic) === 0) return undefined;

	const writable = typeof stream.createWritable === "function" ? stream.createWritable() : stream.writable;

	if (!writable) {
		console.warn("datagram send disabled: WebTransport datagram writable stream is unavailable");
		return undefined;
	}

	try {
		return writable.getWriter();
	} catch (err: unknown) {
		console.warn("datagram send disabled: failed to open WebTransport datagram writer", err);
		return undefined;
	}
}

/**
 * Wire-level QUIC datagram body for moq-lite-05 (§6.4).
 *
 * @module
 */
import { Reader } from "../stream.ts";
import * as Varint from "../varint.ts";

/**
 * A QUIC datagram body: `subscribe (i) | sequence (i) | timestamp (i) | payload (b)`.
 *
 * The payload runs to the datagram boundary, so unlike a size-prefixed lite message there is no
 * inner length prefix. Mirrors the Rust `lite::Datagram`; the model counterpart is
 * {@link Datagram} in `../datagram.ts`.
 */
export class Datagram {
	/** Subscribe ID this datagram is delivered on. */
	subscribe: bigint;
	/** Group sequence number (shared with the track's group namespace). */
	sequence: number;
	/** Absolute presentation timestamp, in the track's negotiated timescale. */
	timestamp: number;
	/** The frame payload, delimited by the datagram boundary. */
	payload: Uint8Array;

	constructor(subscribe: bigint, sequence: number, timestamp: number, payload: Uint8Array) {
		this.subscribe = subscribe;
		this.sequence = sequence;
		this.timestamp = timestamp;
		this.payload = payload;
	}

	/** Encode the body to a single `Uint8Array` (no length prefix; the datagram boundary delimits it). */
	encode(): Uint8Array {
		const subscribe = Varint.encodeTo(new ArrayBuffer(8), this.subscribe);
		const sequence = Varint.encodeTo(new ArrayBuffer(8), this.sequence);
		const timestamp = Varint.encodeTo(new ArrayBuffer(8), this.timestamp);

		const out = new Uint8Array(
			subscribe.byteLength + sequence.byteLength + timestamp.byteLength + this.payload.byteLength,
		);
		let offset = 0;
		out.set(subscribe, offset);
		offset += subscribe.byteLength;
		out.set(sequence, offset);
		offset += sequence.byteLength;
		out.set(timestamp, offset);
		offset += timestamp.byteLength;
		out.set(this.payload, offset);
		return out;
	}

	/** Decode a datagram body from the raw bytes of one QUIC datagram. */
	static async decode(data: Uint8Array): Promise<Datagram> {
		const r = new Reader(undefined, data);
		const subscribe = await r.u62();
		const sequence = await r.u53();
		const timestamp = await r.u53();
		const payload = await r.readAll();
		return new Datagram(subscribe, sequence, timestamp, payload);
	}
}

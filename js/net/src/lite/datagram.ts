/**
 * Wire-level QUIC datagram body for moq-lite-05 (§6.4).
 *
 * @module
 */
import { encodeVarint, Reader } from "../stream.ts";
import { type Version, wireTimescale } from "./version.ts";

/**
 * A QUIC datagram body: `subscribe (i) | sequence (i) | [timestamp (i)] | payload (b)`, with the
 * timestamp present only when the subscription's track carries one (see `wireTimescale`).
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
	/** Absolute presentation timestamp in the track's wire timescale, or undefined on a track that carries none. */
	timestamp: number | undefined;
	/** The frame payload, delimited by the datagram boundary. */
	payload: Uint8Array;

	constructor(subscribe: bigint, sequence: number, timestamp: number | undefined, payload: Uint8Array) {
		this.subscribe = subscribe;
		this.sequence = sequence;
		this.timestamp = timestamp;
		this.payload = payload;
	}

	/** Encode the body to a single `Uint8Array` (no length prefix; the datagram boundary delimits it). */
	encode(version: Version): Uint8Array {
		// A version that always carries a timescale has no untimed body.
		if (this.timestamp === undefined && wireTimescale(version, undefined) !== undefined) {
			throw new Error(`version ${version} has no untimed datagram`);
		}

		const subscribe = encodeVarint(this.subscribe, version);
		const sequence = encodeVarint(this.sequence, version);
		const timestamp = this.timestamp === undefined ? new Uint8Array() : encodeVarint(this.timestamp, version);

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

	/**
	 * Decode a datagram body from the raw bytes of one QUIC datagram. `timed` reports whether the
	 * track of the given subscribe ID carries a timestamp, which the body itself does not say.
	 */
	static async decode(data: Uint8Array, version: Version, timed: (subscribe: bigint) => boolean): Promise<Datagram> {
		const r = new Reader(undefined, data, version);
		const subscribe = await r.u62();
		const sequence = await r.u53();
		const timestamp = timed(subscribe) ? await r.u53() : undefined;
		const payload = await r.readAll();
		return new Datagram(subscribe, sequence, timestamp, payload);
	}
}

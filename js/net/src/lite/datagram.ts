/**
 * Wire-level QUIC datagram bodies for moq-lite.
 *
 * @module
 */
import { encodeVarint, Reader } from "../stream.ts";
import { hasCompression, hasPadding, type Version, wireTimescale } from "./version.ts";

/**
 * A QUIC datagram body: `subscribe (i) | sequence (i) | [timestamp (i)] | [inflated (i)] | payload (b)`, with the
 * timestamp present only when the subscription's track carries one (see `wireTimescale`),
 * and the inflated size only on a compressed lite-07 track.
 * Lite-07 prefixes group datagrams with type 0x0; type 0x1 is discarded padding.
 *
 * The payload runs to the datagram boundary, so unlike a size-prefixed lite message there is no
 * inner length prefix. Mirrors the Rust `lite::Datagram`; the model counterpart is
 * {@link Datagram} in `../datagram.ts`.
 */
export class Datagram {
	/** The largest encoded datagram body a lite hop carries. */
	static readonly MAX_SIZE = 1200;

	/** Subscribe ID this datagram is delivered on. */
	subscribe: bigint;
	/** Group sequence number (shared with the track's group namespace). */
	sequence: number;
	/** Absolute presentation timestamp in the track's wire timescale, or undefined on a track that carries none. */
	timestamp: number | undefined;
	/** The frame payload, delimited by the datagram boundary. */
	payload: Uint8Array;
	/** Plain payload length on a compressed lite-07 track. */
	inflated?: number;

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

		if (this.inflated !== undefined && !hasCompression(version))
			throw new Error("compressed datagram needs lite-07");
		const inflated = this.inflated === undefined ? new Uint8Array() : encodeVarint(this.inflated, version);
		const subscribe = encodeVarint(this.subscribe, version);
		const sequence = encodeVarint(this.sequence, version);
		const timestamp = this.timestamp === undefined ? new Uint8Array() : encodeVarint(this.timestamp, version);
		const type = hasPadding(version) ? encodeVarint(0x0, version) : new Uint8Array();

		const out = new Uint8Array(
			type.byteLength +
				subscribe.byteLength +
				sequence.byteLength +
				timestamp.byteLength +
				inflated.length +
				this.payload.byteLength,
		);
		let offset = 0;
		out.set(type, offset);
		offset += type.byteLength;
		out.set(subscribe, offset);
		offset += subscribe.byteLength;
		out.set(sequence, offset);
		offset += sequence.byteLength;
		out.set(timestamp, offset);
		offset += timestamp.byteLength;
		out.set(inflated, offset);
		offset += inflated.length;
		out.set(this.payload, offset);
		return out;
	}

	/**
	 * Decode a group datagram or discard padding with `undefined`. `timed` reports whether the
	 * track of the given subscribe ID carries a timestamp, which the body itself does not say.
	 */
	static async decode(
		data: Uint8Array,
		version: Version,
		framing: (subscribe: bigint) => { timed: boolean; compressed?: boolean },
	): Promise<Datagram | undefined> {
		const r = new Reader(undefined, data, version);
		if (hasPadding(version)) {
			const type = await r.u62();
			if (type === 1n) return;
			if (type !== 0n) throw new Error(`unknown datagram type: ${type}`);
		}
		const subscribe = await r.u62();
		const sequence = await r.u53();
		const { timed, compressed } = framing(subscribe);
		const timestamp = timed ? await r.u53() : undefined;
		const inflated = hasCompression(version) && compressed ? await r.u53() : undefined;
		const payload = await r.readAll();
		const result = new Datagram(subscribe, sequence, timestamp, payload);
		result.inflated = inflated;
		return result;
	}
}

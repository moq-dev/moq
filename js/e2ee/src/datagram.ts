import { type Time, Varint } from "@moq/net";
import { MAX_DATAGRAM_BODY, TAG_LEN } from "./constants.ts";
import { Failure } from "./error.ts";

/** Inputs for a moq-lite datagram header. */
export interface DatagramHeader {
	/** Subscribe ID the datagram will be delivered on. */
	subscribe: bigint | number;
	/** Datagram sequence (the nonce's group half). */
	sequence: number;
	/** Presentation timestamp as it will be encoded on the wire. */
	timestamp: number;
}

/** QUIC-varint size of a moq-lite datagram header (subscribe, sequence, timestamp). */
export function datagramHeaderSize(header: DatagramHeader): number {
	return Varint.encode(header.subscribe).byteLength + Varint.size(header.sequence) + Varint.size(header.timestamp);
}

/**
 * Maximum ciphertext length for this datagram, `1200 - header`.
 * Plaintext must be at most this minus the 16-byte tag.
 */
export function datagramPayloadLimit(header: DatagramHeader): number {
	const headerSize = datagramHeaderSize(header);
	if (headerSize >= MAX_DATAGRAM_BODY) throw new Failure("oversize");
	return MAX_DATAGRAM_BODY - headerSize;
}

/** Maximum plaintext for this datagram. */
export function datagramPlaintextLimit(header: DatagramHeader): number {
	const limit = datagramPayloadLimit(header) - TAG_LEN;
	if (limit < 0) throw new Failure("oversize");
	return limit;
}

type DatagramWriter = {
	insertDatagram?: (sequence: number, timestamp: Time.Timestamp, payload: Uint8Array) => void;
	writeDatagram: (datagram: { sequence: number; timestamp: Time.Timestamp; payload: Uint8Array }) => void;
};

/**
 * Insert a datagram at an explicit sequence.
 *
 * Uses `insertDatagram` when the sibling moq-net rename has landed; otherwise
 * `writeDatagram`. This package does not rename the transport API.
 */
export function insertDatagram(
	track: DatagramWriter,
	sequence: number,
	timestamp: Time.Timestamp,
	payload: Uint8Array,
): void {
	if (typeof track.insertDatagram === "function") {
		track.insertDatagram(sequence, timestamp, payload);
		return;
	}
	track.writeDatagram({ sequence, timestamp, payload });
}

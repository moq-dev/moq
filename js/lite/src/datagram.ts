/**
 * A single datagram: opaque payload with a sequence number.
 *
 * moq-lite-04-datagrams ignores the sequence number for delivery semantics; the field is
 * preserved so the same model works under a future `moq-transport` adapter.
 */
export class Datagram {
	readonly sequence: number;
	readonly payload: Uint8Array;

	constructor(sequence: number, payload: Uint8Array) {
		this.sequence = sequence;
		this.payload = payload;
	}
}

/** Datagrams older than this are evicted from the cache. */
export const MAX_DATAGRAM_AGE_MS = 33;

/** Maximum payload size per datagram, in bytes. */
export const MAX_DATAGRAM_PAYLOAD = 1200;

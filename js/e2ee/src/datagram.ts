/**
 * Decrypted datagrams and the events a protected datagram read yields.
 *
 * @module
 */
import type { Time } from "@moq/net";

/** A decrypted datagram. */
export interface Datagram {
	/** Per-track sequence, shared with the group namespace. */
	sequence: bigint;
	/** Presentation timestamp, or `undefined` when untimed. Not authenticated. */
	timestamp?: Time.Timestamp;
	/** Decrypted application bytes. */
	plaintext: Uint8Array;
}

/**
 * Outcome of reading one protected datagram.
 *
 * - `datagram`: authenticated plaintext.
 * - `authentication`: the datagram failed to authenticate; dropped, track continues.
 * - `duplicate`: the sequence was already opened inside the retained window; dropped, track continues.
 * - `identity`: the sequence is above `2^53-1`; dropped, track continues.
 * - `oversize`: the payload is shorter than the tag or longer than the datagram cap; dropped, track continues.
 */
export type Event =
	| { kind: "datagram"; datagram: Datagram }
	| { kind: "authentication"; sequence: bigint }
	| { kind: "duplicate"; sequence: bigint }
	| { kind: "identity"; sequence: bigint }
	| { kind: "oversize"; sequence: bigint };

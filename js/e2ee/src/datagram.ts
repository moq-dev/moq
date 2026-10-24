/**
 * Decrypted datagrams and the events a protected datagram read yields.
 *
 * @module
 */
import type { Time } from "@moq/net";

/** A decrypted datagram. */
export interface Datagram {
	/** Per-track sequence, shared with the group namespace. */
	sequence: number;
	/** Presentation timestamp, or `undefined` when untimed. Not authenticated. */
	timestamp?: Time.Timestamp;
	/** Decrypted application bytes. */
	plaintext: Uint8Array;
}

/**
 * Outcome of reading one protected datagram.
 *
 * - `datagram`: authenticated plaintext.
 * - `authentication`: the datagram failed to authenticate, or had a sequence or size no publisher
 *   could seal; it was dropped and the track continues.
 * - `duplicate`: the sequence was already opened inside the retained window; dropped, track continues.
 */
export type Event =
	| { kind: "datagram"; datagram: Datagram }
	| { kind: "authentication"; sequence: number }
	| { kind: "duplicate"; sequence: number };

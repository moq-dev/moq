/**
 * A datagram: a single unreliable payload on a track, parallel to groups.
 *
 * @module
 */
import type { Timestamp } from "./time.ts";

/**
 * Maximum datagram payload size, in bytes.
 *
 * A datagram body (sequence + timestamp + payload) must fit in a single QUIC datagram without IP
 * fragmentation, so the payload is capped below the minimum path MTU. Producers reject a larger
 * payload; there is no group fallback, so keep datagram payloads small (e.g. a single audio frame).
 * Mirrors the Rust `MAX_DATAGRAM_PAYLOAD`.
 */
export const MAX_DATAGRAM_PAYLOAD = 1200;

/**
 * A single unreliable payload on a track: a sequence number, a presentation timestamp, and the bytes.
 *
 * Unlike a {@link Group} (an ordered stream of frames over a QUIC stream), a datagram is one
 * self-contained payload carried in a single QUIC datagram: best-effort, unordered, never
 * retransmitted. It shares the track's monotonic sequence-number namespace with groups but is
 * otherwise independent. Mirrors the Rust `Datagram`.
 */
export interface Datagram {
	/** Per-track sequence number, shared with the group namespace. */
	sequence: number;
	/** Presentation timestamp in the track's timescale. */
	timestamp: Timestamp;
	/** The datagram payload. */
	payload: Uint8Array;
}

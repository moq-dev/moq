/**
 * A pseudo-random origin ID for loop detection on the wire.
 *
 * Must be non-zero and fit in 62 bits (the wire varint limit). We use
 * `bigint` rather than `number` because Rust peers can emit the full
 * 62-bit range, which exceeds JS's safe integer limit.
 * Collisions are vanishingly rare at this size and loop detection is
 * best-effort, so `crypto.getRandomValues` is overkill — `Math.random`
 * would technically suffice, but we use crypto for slightly better
 * distribution at negligible cost.
 */
export function randomOriginId(): bigint {
	const buf = new BigUint64Array(1);
	crypto.getRandomValues(buf);
	// Mask to 62 bits.
	const id = buf[0] & 0x3fff_ffff_ffff_ffffn;
	// Guard against the (astronomically unlikely) zero draw.
	return id === 0n ? 1n : id;
}

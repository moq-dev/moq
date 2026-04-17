/**
 * A pseudo-random origin ID for loop detection on the wire.
 *
 * Must be non-zero and fit in a 53-bit JS safe integer (the wire format
 * accepts up to 62 bits, but JS `number` tops out at 2^53 - 1).
 * Collisions are vanishingly rare at this size and loop detection is
 * best-effort, so `crypto.getRandomValues` is overkill — `Math.random`
 * would technically suffice, but we use crypto for slightly better
 * distribution at negligible cost.
 */
export function randomOriginId(): number {
	const buf = new Uint32Array(2);
	crypto.getRandomValues(buf);
	// Compose a 53-bit value: 21 high bits from buf[1] + 32 low bits from buf[0].
	const high = buf[1] & 0x1f_ffff; // 21 bits
	const low = buf[0];
	const id = high * 0x1_0000_0000 + low;
	// Guard against the (astronomically unlikely) zero draw.
	return id === 0 ? 1 : id;
}

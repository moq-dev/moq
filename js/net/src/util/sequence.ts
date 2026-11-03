// Group sequences are bigints, which `Math.min` and `Math.max` refuse.

/** The smaller of two sequences. */
export function min(a: bigint, b: bigint): bigint {
	return a < b ? a : b;
}

/** The larger of two sequences. */
export function max(a: bigint, b: bigint): bigint {
	return a > b ? a : b;
}

/** `a - b`, or 0 when `b` is at or past `a`. */
export function saturatingSub(a: bigint, b: bigint): bigint {
	return a > b ? a - b : 0n;
}

/** An exclusive end past every group a wire can carry (2^64), which an open run ends at. */
export const UNBOUNDED = 1n << 64n;

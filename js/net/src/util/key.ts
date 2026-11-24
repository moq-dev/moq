// Group keys are bigints, which `Math.min` and `Math.max` refuse.

/** The smaller of two keys. */
export function min(a: bigint, b: bigint): bigint {
	return a < b ? a : b;
}

/** The larger of two keys. */
export function max(a: bigint, b: bigint): bigint {
	return a > b ? a : b;
}

/** `a - b`, or 0 when `b` is at or past `a`. */
export function saturatingSub(a: bigint, b: bigint): bigint {
	return a > b ? a - b : 0n;
}

/** An exclusive end past every group a wire can carry (2^64), which an open run ends at. */
export const UNBOUNDED = 1n << 64n;

/** The reserved group key, 2^64-1: no group is created there, so every group fits a half-open range. */
export const RESERVED = UNBOUNDED - 1n;

/** Refuse a group key a u64 cannot hold, or that is not a bigint, either of which a plain JS caller can pass. */
export function checkKey(key: bigint): bigint {
	if (typeof key !== "bigint") throw new TypeError(`group sequence must be a bigint: ${String(key)}`);
	if (key < 0n) throw new RangeError(`group sequence must be non-negative: ${key}`);
	if (key >= UNBOUNDED) throw new RangeError(`group sequence exceeds 2^64-1: ${key}`);
	return key;
}

/** Refuse an exclusive end of a run of group keys that is not a bigint in `[0, 2^64-1]`, so a run never names the reserved group. */
export function checkEnd(end: bigint): bigint {
	if (typeof end !== "bigint") throw new TypeError(`group end must be a bigint: ${String(end)}`);
	if (end < 0n) throw new RangeError(`group end must be non-negative: ${end}`);
	if (end > RESERVED) throw new RangeError(`group end exceeds 2^64-1: ${end}`);
	return end;
}

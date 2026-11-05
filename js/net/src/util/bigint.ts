const MAX_SAFE_INTEGER = BigInt(Number.MAX_SAFE_INTEGER);

/** Convert an unsigned integer to a model number, refusing it before any rounding. */
export function toNumber(value: bigint): number {
	if (value < 0n || value > MAX_SAFE_INTEGER) throw new RangeError(`integer outside the safe number range: ${value}`);
	return Number(value);
}

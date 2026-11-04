import type { Reader, Writer } from "../stream.ts";

/** Encode optional milliseconds, with zero meaning absent and a present value offset by one. */
export async function encodeOptional(w: Writer, value: number | undefined): Promise<void> {
	if (value !== undefined && (!Number.isSafeInteger(value) || value < 0))
		throw new RangeError("duration exceeds safe milliseconds");
	await w.u62(value === undefined ? 0n : BigInt(value) + 1n);
}

/** Remove the optional offset before converting the duration to safe numeric milliseconds. */
export async function decodeOptional(r: Reader): Promise<number | undefined> {
	const value = await r.u62();
	if (value > BigInt(Number.MAX_SAFE_INTEGER) + 1n) throw new RangeError("duration exceeds safe milliseconds");
	return value === 0n ? undefined : Number(value - 1n);
}

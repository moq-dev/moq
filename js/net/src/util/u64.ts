// An unsigned 64-bit integer as two u32 halves, so nothing on the hot path needs a BigInt.
// Package-internal. Varints are only its wire encoding; see ./varint.ts.

/** 2^32, the weight of the upper half. */
export const POW32 = 0x1_0000_0000;
// An upper half at or above this is past `Number.MAX_SAFE_INTEGER`.
const SAFE_HI = 2 ** 21;

/** Join two halves into a `number`, throwing if it is above `Number.MAX_SAFE_INTEGER` rather than rounding. */
export function toNumber(hi: number, lo: number): number {
	if (hi >= SAFE_HI) throw new RangeError(`value larger than 53-bits: ${toBigInt(hi, lo)}`);
	return hi * POW32 + lo;
}

/** Join two halves into a bigint, exact up to 64 bits. */
export function toBigInt(hi: number, lo: number): bigint {
	return hi === 0 ? BigInt(lo) : (BigInt(hi) << 32n) | BigInt(lo);
}

/**
 * An unsigned 64-bit integer, held as two 32-bit halves so it encodes and decodes without a BigInt.
 * Converting to a `number` throws past 2^53 rather than rounding.
 */
export class U64 {
	/** The value 0. */
	static readonly ZERO = new U64(0, 0);
	/** The largest value, 2^64 - 1. */
	static readonly MAX = new U64(POW32 - 1, POW32 - 1);

	/** The upper 32 bits. */
	readonly hi: number;
	/** The lower 32 bits. */
	readonly lo: number;

	/** Join the upper and lower 32 bits, throwing if either is not a u32. */
	constructor(hi: number, lo: number) {
		if (!Number.isInteger(hi) || hi < 0 || hi >= POW32) throw new RangeError(`invalid upper half: ${hi}`);
		if (!Number.isInteger(lo) || lo < 0 || lo >= POW32) throw new RangeError(`invalid lower half: ${lo}`);
		this.hi = hi;
		this.lo = lo;
	}

	/** Convert a non-negative safe integer, throwing on anything else. */
	static fromNumber(v: number): U64 {
		if (!Number.isSafeInteger(v) || v < 0) throw new RangeError(`invalid u64: ${v}`);
		return new U64(Math.floor(v / POW32), v >>> 0);
	}

	/** Convert a bigint, throwing unless it is in [0, 2^64). */
	static fromBigInt(v: bigint): U64 {
		if (v < 0n || v >> 64n) throw new RangeError(`invalid u64: ${v}`);
		return new U64(Number(v >> 32n), Number(v & 0xffffffffn));
	}

	/** Convert to a `number`, throwing if it is above `Number.MAX_SAFE_INTEGER`. */
	toNumber(): number {
		return toNumber(this.hi, this.lo);
	}

	/** Convert to a bigint, exactly. */
	toBigInt(): bigint {
		return toBigInt(this.hi, this.lo);
	}

	/** The value in decimal. */
	toString(): string {
		return this.hi < SAFE_HI ? String(this.hi * POW32 + this.lo) : this.toBigInt().toString();
	}

	/** Negative if this is less than `other`, zero if equal, positive if greater. */
	compare(other: U64): number {
		return this.hi - other.hi || this.lo - other.lo;
	}

	/** Whether this equals `other`. */
	equals(other: U64): boolean {
		return this.hi === other.hi && this.lo === other.lo;
	}

	/** This plus a non-negative safe integer, throwing if the sum reaches 2^64. */
	add(delta: number): U64 {
		if (!Number.isSafeInteger(delta) || delta < 0) throw new RangeError(`invalid delta: ${delta}`);
		const lo = this.lo + (delta >>> 0);
		const hi = this.hi + Math.floor(delta / POW32) + (lo >= POW32 ? 1 : 0);
		if (hi >= POW32) throw new RangeError(`overflow, ${this} + ${delta} exceeds 64 bits`);
		return new U64(hi, lo >>> 0);
	}
}

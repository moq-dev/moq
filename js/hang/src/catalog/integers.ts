import * as z from "zod/mini";

/**
 * Branded type for 8-bit unsigned integers (0-255)
 */
export const u8Schema = z.number().check(z.int(), z.nonnegative(), z.lte(255)).brand("u8");

export type U8 = z.infer<typeof u8Schema>;

/**
 * Branded type for 53-bit unsigned integers (JavaScript's MAX_SAFE_INTEGER)
 * This represents the maximum safe integer in JavaScript (2^53 - 1)
 */
export const u53Schema = z.number().check(z.int(), z.nonnegative(), z.lte(Number.MAX_SAFE_INTEGER)).brand("u53");

export type U53 = z.infer<typeof u53Schema>;

/**
 * Convenience function to create a u8 value
 */
export function u8(value: number): U8 {
	return u8Schema.parse(value);
}

/**
 * Convenience function to create a u53 value
 */
export function u53(value: number): U53 {
	return u53Schema.parse(value);
}

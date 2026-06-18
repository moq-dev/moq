import * as z from "zod/mini";

/**
 * A relay origin id, encoded as a 62-bit varint on the wire.
 *
 * The {@link OriginSchema} validates any incoming value and brands it so the
 * type system enforces "only validated origins flow into hop lists." Internal
 * code that synthesizes an id (e.g. {@link randomOrigin}) uses
 * `OriginSchema.parse(...)` to produce a branded value from the raw bigint.
 */
export const OriginSchema = z
	.bigint()
	.check(z.refine((value) => value >= 0n && value < 1n << 62n, "Origin must be a non-negative 62-bit integer"))
	.brand("Origin");

export type Origin = z.infer<typeof OriginSchema>;

/**
 * Generate a fresh origin with a random non-zero id.
 *
 * `crypto.getRandomValues` is overkill for best-effort loop detection, but
 * used for slightly better distribution than `Math.random` at negligible cost.
 *
 * TEMPORARY: the wire format allows 62 bits, but older `@moq/lite` JS clients
 * decode `AnnounceInterest.exclude_hop` as a u53 (number) and throw on anything
 * > 2^53-1. To keep those clients alive against fresh peers, we cap the random
 * id at 53 bits. Restore to 62 bits once the u62 fix has propagated to deployed
 * bundles. Mirrors `Origin::random` in rs/moq-net.
 */
export function randomOrigin(): Origin {
	const buf = new BigUint64Array(1);
	crypto.getRandomValues(buf);
	// Mask to 53 bits.
	const raw = buf[0] & 0x1f_ffff_ffff_ffffn;
	// Guard against the (astronomically unlikely) zero draw.
	return OriginSchema.parse(raw === 0n ? 1n : raw);
}

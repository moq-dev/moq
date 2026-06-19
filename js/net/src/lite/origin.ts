import * as z from "zod/mini";

/**
 * A relay origin id (Hop ID), randomly assigned and carried in the hop chain.
 *
 * On the wire lite-05+ encodes it as a fixed-width 64-bit integer (the full
 * 64-bit space); older versions used a 62-bit varint. The {@link OriginSchema}
 * validates any incoming value and brands it so the type system enforces "only
 * validated origins flow into hop lists." Internal code that synthesizes an id
 * (e.g. {@link randomOrigin}) uses `OriginSchema.parse(...)` to produce a
 * branded value from the raw bigint.
 */
export const OriginSchema = z
	.bigint()
	.check(z.refine((value) => value >= 0n && value < 1n << 64n, "Origin must be a non-negative 64-bit integer"))
	.brand("Origin");

export type Origin = z.infer<typeof OriginSchema>;

/**
 * Generate a fresh origin with a random non-zero id.
 *
 * Masked to 62 bits: the same id is encoded as our self/exclude Hop ID, which on
 * lite-04 is still a 62-bit varint (lite-05 carries the full 64-bit space). Staying
 * within 62 bits keeps one generated id valid across both negotiated versions.
 *
 * `crypto.getRandomValues` is overkill for best-effort loop detection, but
 * used for slightly better distribution than `Math.random` at negligible cost.
 */
export function randomOrigin(): Origin {
	const buf = new BigUint64Array(1);
	crypto.getRandomValues(buf);
	// Mask to 62 bits.
	const raw = buf[0] & 0x3fff_ffff_ffff_ffffn;
	// Guard against the (astronomically unlikely) zero draw.
	return OriginSchema.parse(raw === 0n ? 1n : raw);
}

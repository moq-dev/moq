import * as z from "zod/mini";

export const ClaimsSchema = z
	.object({
		root: z.string(),
		put: z.optional(z.union([z.string(), z.array(z.string())])),
		cluster: z.optional(z.boolean()),
		get: z.optional(z.union([z.string(), z.array(z.string())])),
		exp: z.optional(z.number()),
		iat: z.optional(z.number()),
	})
	.check(
		z.refine((data) => data.put !== undefined || data.get !== undefined, {
			message: "Either put or get must be specified",
		}),
	);

/**
 * JWT claims structure for moq-token
 */
export type Claims = z.infer<typeof ClaimsSchema>;

/**
 * Validate claims structure and business rules
 */
export function validateClaims(claims: Claims): void {
	if (claims.put === undefined && claims.get === undefined) {
		throw new Error("no put or get paths specified; token is useless");
	}
}

// Export with lowercase for backward compatibility
export const claimsSchema = ClaimsSchema;

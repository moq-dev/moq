import * as z from "zod/mini";

export const ClaimsSchema = z
	.object({
		root: z.string(),
		put: z.optional(z.union([z.string(), z.array(z.string())])),
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

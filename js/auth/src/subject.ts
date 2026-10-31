/**
 * Who a token was issued to.
 *
 * @module
 */

import * as z from "@zod/mini";

/**
 * Who a token was issued to: its subject, qualified by the issuer that named it, so two
 * issuers' subjects never collide. Mirrors `moq_auth::Subject` in the Rust crate.
 */
export const SubjectSchema = z.object({
	/** The token's issuer (`iss`), absent when it names none. */
	issuer: z.optional(z.string()),
	/** The token's subject (`sub`), unique within its issuer. */
	name: z.string(),
});
/** A value parsed by {@link SubjectSchema}. */
export type Subject = z.infer<typeof SubjectSchema>;

import { z } from "zod";

/**
 * Supported JWT algorithms.
 */
export const AlgorithmSchema = z.enum([
	"HS256",
	"HS384",
	"HS512",
	"ES256",
	"ES384",
	"RS256",
	"RS384",
	"RS512",
	"PS256",
	"PS384",
	"PS512",
	"EdDSA",
]);
export type Algorithm = z.infer<typeof AlgorithmSchema>;

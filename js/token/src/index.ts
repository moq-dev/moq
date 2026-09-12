/**
 * JWT token generation and validation for MoQ authentication.
 *
 * Create and verify JWT tokens used for authorizing publish/subscribe operations in
 * MoQ. Tokens specify which broadcast paths a client can publish to and consume from.
 *
 * See {@link Claims} for the claims structure and {@link Key} for key management.
 * Path grants use {@link Pattern} from `@moq/pattern`: the same grammar `@moq/net`
 * re-exports. New minting should construct patterns rather than prefixes; missing
 * `v` on persisted claims stays v0 prefix semantics.
 *
 * @module
 */

export {
	compareSpecificity,
	type ErrorCode,
	Pattern,
	PatternError,
	Patterns,
	type Segment,
	type Specificity,
} from "@moq/pattern";
export * from "./algorithm.ts";
export * from "./claims.ts";
export * from "./generate.ts";
export * from "./key.ts";
export * from "./set.ts";

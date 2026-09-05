/**
 * JWT token generation and validation for MoQ authentication.
 *
 * Create and verify JWT tokens used for authorizing publish/subscribe operations in
 * MoQ. Tokens specify which broadcast paths a client can publish to and consume from.
 *
 * See {@link Claims} for the claims structure and {@link Key} for key management.
 *
 * @module
 */

export type { Segment as PatternSegment, Specificity } from "@moq/path";
/** Re-export of {@link https://jsr.io/@moq/path | @moq/path}: the path patterns grants are written in. */
export { compareSpecificity, Pattern, PatternError, Patterns } from "@moq/path";
export * from "./algorithm.ts";
export * from "./claims.ts";
export * from "./generate.ts";
export * from "./key.ts";
export * from "./set.ts";

/**
 * The authorization contract for Media over QUIC.
 *
 * A relay asks one question per session, "may this connect?", and this package holds
 * every piece of the answer: the {@link Request} it POSTs and the {@link Grant} it
 * gets back, and the JWT {@link Claims} a client presents in its query with the
 * {@link Key} that signs and verifies it.
 *
 * Grants and claims name paths with patterns from `@moq/pattern`, re-exported here:
 * `foo` is one broadcast, `foo/**` is a subtree, `**` is everything.
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
export * from "./contract.ts";
export * from "./generate.ts";
export * from "./key.ts";
export * from "./set.ts";

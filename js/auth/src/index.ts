/**
 * The authorization contract for Media over QUIC.
 *
 * A relay asks one question per session, "may this connect?", and this package holds
 * every piece of the answer: the {@link Request} it POSTs and the {@link Grant} it
 * gets back, and the JWT {@link Claims} a client presents in its query with the
 * {@link Key} that signs and verifies it.
 *
 * Grants and claims name paths with prefixes from `@moq/pattern`, re-exported here:
 * `foo` is `foo` and everything beneath it, and the empty prefix is everything. A grant
 * takes no wildcard: `foo/**` and `**` are refused, written `foo` and `""` instead.
 *
 * @module
 */

export { InvalidPrefix, Prefix, Prefixes } from "@moq/pattern";
export * from "./algorithm.ts";
export * from "./claims.ts";
export * from "./contract.ts";
export * from "./key.ts";
export * from "./set.ts";

/**
 * WebCodecs-based media layer on top of `@moq/net`: catalog, container, and helpers
 * for publishing and consuming live audio/video over MoQ.
 *
 * @module
 */

export * as Net from "@moq/net";
/** @deprecated Use `Net` instead. */
export * as Moq from "@moq/net";
export * as Signals from "@moq/signals";
export * as Catalog from "./catalog";
export * as Container from "./container";

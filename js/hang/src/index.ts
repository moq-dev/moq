/**
 * WebCodecs-based media layer on top of `@moq/net`: catalog, container, and helpers
 * for publishing and consuming live audio/video over MoQ.
 *
 * @module
 */

/** Re-export of {@link https://jsr.io/@moq/net | @moq/net}, the transport this package builds on. */
export * as Net from "@moq/net";
/** Re-export of {@link https://jsr.io/@moq/signals | @moq/signals}, the reactive primitives used throughout this package. */
export * as Signals from "@moq/signals";
/** The catalog schema describing a broadcast's tracks. */
export * as Catalog from "./catalog";
/** Per-frame container formats, with consumers and producers to read and write them. */
export * as Container from "./container";
/** Solicited viewer feedback schemas and broadcast names. */
export * as Echo from "./echo";
/** Client health graded from two consecutive media reports. */
export * as Health from "./health";
/** A rendition's HLS segment records, each segment's first group and pts. */
export * as Hls from "./hls";
/** The compressed snapshot track a stats or feedback report is written to. */
export * as Report from "./report";
/** Publisher media and transport snapshot schemas. */
export * as Stats from "./stats";

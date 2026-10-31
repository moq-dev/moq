/**
 * Per-frame container formats (timestamp plus codec bitstream): the legacy varint
 * container and LOC, with consumers and producers to read/write them.
 *
 * @module
 */

/** Re-export of {@link https://jsr.io/@moq/loc | @moq/loc}, the LOC container. */
export * as Loc from "@moq/loc";
export { Consumer, type ConsumerProps, DEFAULT_MAX_BUFFER, type Read } from "./consumer";
export type { Format } from "./format";
/** The legacy varint container. */
export * as Legacy from "./legacy";
export { type TrackInfoOptions, trackInfo } from "./track";
export * from "./types";

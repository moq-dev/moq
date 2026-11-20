/**
 * Per-frame container formats (timestamp plus codec bitstream): the legacy varint
 * container and LOC, with consumers and producers to read/write them.
 *
 * @module
 */

/** Re-export of {@link https://www.npmjs.com/package/@moq/loc | @moq/loc}, the LOC container. */
export * as Loc from "@moq/loc";
export { Consumer, type ConsumerProps, DELAY_MAX, type Read } from "./consumer.ts";
export type { Format } from "./format.ts";
/** The legacy varint container. */
export * as Legacy from "./legacy.ts";
export { MAX_AGE, type TrackInfoOptions, trackInfo } from "./track.ts";
export * from "./types.ts";

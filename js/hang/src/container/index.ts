/**
 * Per-frame container formats (timestamp plus codec bitstream): the legacy varint
 * container, CMAF (fMP4), and LOC, with consumers and producers to read/write them.
 *
 * @module
 */

export * as Loc from "@moq/loc";
export * as Cmaf from "./cmaf";
export { Consumer, type ConsumerProps } from "./consumer";
export type { Format } from "./format";
export * as Legacy from "./legacy";
export * from "./types";

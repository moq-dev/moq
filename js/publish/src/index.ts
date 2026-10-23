/**
 * Capture media, encode it, and publish it as a broadcast.
 *
 * The JS API: compose {@link Broadcast}, a `Source`, and the `Video`/`Audio` encoders yourself.
 * For a drop-in element, import `@moq/publish/element` instead.
 *
 * @module
 */

/** Re-export of {@link https://jsr.io/@moq/clock | @moq/clock}, the time units this package's signatures take. */
export * as Clock from "@moq/clock";
/** Re-export of {@link https://jsr.io/@moq/hang | @moq/hang}, the media layer. */
export * as Hang from "@moq/hang";
/** Re-export of {@link https://jsr.io/@moq/net | @moq/net}, the transport. */
export * as Net from "@moq/net";
/** Re-export of {@link https://jsr.io/@moq/signals | @moq/signals}, the reactive primitives used throughout this package. */
export * as Signals from "@moq/signals";
export { assets } from "./assets";
/** Audio capture and encoding. */
export * as Audio from "./audio";
export * from "./broadcast";
export * from "./catalog";
/** The local canvas preview. */
export * as Preview from "./preview";
export * from "./rendition";
/** Capture sources: camera, microphone, screen, and file. */
export * as Source from "./source";
/** Video capture and encoding. */
export * as Video from "./video";

// NOTE: element is not exported from this module
// You have to import it from @moq/publish/element instead.

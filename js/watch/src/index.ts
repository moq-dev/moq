/**
 * Subscribe to a broadcast, decode it, and render it.
 *
 * Use {@link Player} for a complete headless pipeline, or compose {@link Broadcast}, {@link Sync},
 * and the per-track components yourself. For an element, import `@moq/watch/element` instead.
 *
 * @module
 */
/** Re-export of {@link https://jsr.io/@moq/clock | @moq/clock}, the time units this package's signatures take. */
export * as Clock from "@moq/clock";
/** Re-export of {@link https://jsr.io/@moq/hang | @moq/hang}, the media layer. */
export * as Hang from "@moq/hang";
// Re-exported from @moq/hang so watch consumers can name the decoders' buffered output.
export type { BufferedRange, BufferedRanges } from "@moq/hang/container";
/** Re-export of {@link https://jsr.io/@moq/net | @moq/net}, the transport. */
export * as Net from "@moq/net";
/** Re-export of {@link https://jsr.io/@moq/signals | @moq/signals}, the reactive primitives used throughout this package. */
export * as Signals from "@moq/signals";
export type { Arrival, Arrivals } from "./arrivals";
export { assets } from "./assets";
/** Audio rendition selection, decoding, and playback. */
export * as Audio from "./audio";
export * from "./broadcast";
export * from "./player";
export * from "./sync";
/** Caption and subtitle playback. */
export * as Text from "./text";
/** Video rendition selection, decoding, and rendering. */
export * as Video from "./video";

// NOTE: element is not exported from this module
// You have to import it from @moq/watch/element instead.

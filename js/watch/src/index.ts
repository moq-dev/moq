/**
 * Subscribe to a broadcast, decode it, and render it.
 *
 * Use {@link Player} for a complete headless pipeline, or compose {@link Broadcast}, {@link Sync},
 * and the per-track components yourself. For an element, import `@moq/watch/element` instead.
 *
 * @module
 */
export * as Hang from "@moq/hang";
// Re-exported from @moq/hang so watch consumers can name the decoders' buffered output.
export type { BufferedRange, BufferedRanges } from "@moq/hang/container";
export * as Net from "@moq/net";
export * as Signals from "@moq/signals";
export * as Audio from "./audio";
export * from "./broadcast";
export * from "./player";
export * from "./sync";
export * as Text from "./text";
export * as Video from "./video";

// NOTE: element is not exported from this module
// You have to import it from @moq/watch/element instead.

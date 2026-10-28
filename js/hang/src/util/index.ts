/**
 * Miscellaneous helpers for the hang media layer: AAC and Opus codec constraints, hex encoding,
 * the libav/WebCodecs polyfill, and unlocking Web Audio on a user gesture.
 *
 * @module
 */

/** AAC codec constraints. */
export * as Aac from "./aac";
/** Unlocking Web Audio on a user gesture. */
export * as Gesture from "./gesture";
/** Browser detection for engine-specific workarounds. */
export * as Hacks from "./hacks";
/** Hex encoding and decoding. */
export * as Hex from "./hex";
/** The libav WebCodecs polyfill. */
export * as Libav from "./libav";
/** Opus codec constraints. */
export * as Opus from "./opus";

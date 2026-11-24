/**
 * Miscellaneous helpers for the hang media layer: AAC and Opus codec constraints, hex encoding,
 * self-hosted file directories, the libav/WebCodecs polyfill, and unlocking Web Audio on a user gesture.
 *
 * @module
 */

/** AAC codec constraints. */
export * as Aac from "./aac.ts";
/** A package's directory of self-hosted files. */
export * as Assets from "./assets.ts";
/** Unlocking Web Audio on a user gesture. */
export * as Gesture from "./gesture.ts";
/** Browser detection for engine-specific workarounds. */
export * as Hacks from "./hacks.ts";
/** Hex encoding and decoding. */
export * as Hex from "./hex.ts";
/** The libav WebCodecs polyfill. */
export * as Libav from "./libav.ts";
/** Opus codec constraints. */
export * as Opus from "./opus.ts";

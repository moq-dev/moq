/**
 * libav.js, decoding Opus and AAC; pass it to the WebCodecs polyfill as `LibAV`.
 *
 * @module
 */
import type { LibAVWrapper } from "@libav.js/types";

/** The libav.js wrapper, whose `LibAV()` creates an instance. */
declare const LibAV: LibAVWrapper;
export default LibAV;

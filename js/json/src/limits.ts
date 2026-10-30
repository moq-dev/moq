// Whole-frame reads retain their allocation guard until chunked frame reads are available.
export const MAX_FRAME_BYTES = 32 * 1024 * 1024;

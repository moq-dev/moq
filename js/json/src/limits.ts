/** The largest frame a JSON encoder emits: moq-net's whole-frame guard on the compressed tracks these are meant for. */
export const MAX_FRAME_BYTES = 32 * 1024 * 1024;

/** The live tail moq-net keeps of each group. A group's checkpoint must fit in it with every frame after it. */
export const TAIL_BYTES = 32 * 1024 * 1024;

/**
 * At most what moq-net's tail charges a frame of `size` payload bytes: the plain payload, the
 * DEFLATE slice kept beside it on a compressed track (which outgrows its input by at most a few
 * bytes per block), and a fixed per-frame overhead.
 */
export function tailCharge(size: number): number {
	return 2 * size + Math.ceil(size / 16384) * 5 + 512;
}

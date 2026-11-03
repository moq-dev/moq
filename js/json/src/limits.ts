// moq-net's whole-frame guard on the compressed tracks these are meant for, applied to a slice.
const MAX_SLICE_BYTES = 32 * 1024 * 1024;

// zlib's `deflateBound`, the worst-case slice moq-net checks a local compressed frame against.
function deflateBound(size: number): number {
	return size + Math.floor(size / 4096) + Math.floor(size / 16384) + Math.floor(size / 33554432) + 13;
}

/** The largest frame a JSON encoder emits: the largest moq-net admits on a compressed track. */
export const MAX_FRAME_BYTES = (() => {
	let size = MAX_SLICE_BYTES;
	while (deflateBound(size) > MAX_SLICE_BYTES) size--;
	return size;
})();

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

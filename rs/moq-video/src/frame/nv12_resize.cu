// Box-average NV12 resize, the GPU half of decode::Frame::resize.
//
// One thread per destination pixel (luma) or destination UV pair (chroma).
// Each destination pixel averages its full source box, so arbitrary downscale
// factors don't alias the way a fixed 4-tap bilinear would; at 1:1 it degrades
// to a copy. Ladder rungs never upscale, so upscale quality is irrelevant
// (the box degenerates to nearest-neighbor).
//
// Vendored PTX: this file is compiled offline to nv12_resize.ptx (see the
// comment there for the exact command), which is embedded and JIT-compiled by
// the driver at runtime. Building the crate needs no CUDA toolkit, matching
// the dlopen-only design of the NVENC/NVDEC backends. If you edit this file,
// regenerate the PTX next to it.

extern "C" __global__ void resize_luma(
	const unsigned char *src, unsigned int src_pitch, unsigned int src_w, unsigned int src_h,
	unsigned char *dst, unsigned int dst_pitch, unsigned int dst_w, unsigned int dst_h)
{
	unsigned int x = blockIdx.x * blockDim.x + threadIdx.x;
	unsigned int y = blockIdx.y * blockDim.y + threadIdx.y;
	if (x >= dst_w || y >= dst_h)
		return;

	// The half-open source box covered by this destination pixel, at least
	// one source pixel even when upscaling.
	unsigned int x0 = x * src_w / dst_w;
	unsigned int x1 = (x + 1) * src_w / dst_w;
	if (x1 <= x0)
		x1 = x0 + 1;
	unsigned int y0 = y * src_h / dst_h;
	unsigned int y1 = (y + 1) * src_h / dst_h;
	if (y1 <= y0)
		y1 = y0 + 1;

	unsigned int sum = 0;
	for (unsigned int sy = y0; sy < y1; sy++)
		for (unsigned int sx = x0; sx < x1; sx++)
			sum += src[sy * src_pitch + sx];

	unsigned int n = (x1 - x0) * (y1 - y0);
	dst[y * dst_pitch + x] = (unsigned char)((sum + n / 2) / n);
}

// Same box average over the interleaved UV plane. Widths and x are in UV
// *pairs* (2 bytes each); U and V accumulate separately.
extern "C" __global__ void resize_chroma(
	const unsigned char *src, unsigned int src_pitch, unsigned int src_w, unsigned int src_h,
	unsigned char *dst, unsigned int dst_pitch, unsigned int dst_w, unsigned int dst_h)
{
	unsigned int x = blockIdx.x * blockDim.x + threadIdx.x;
	unsigned int y = blockIdx.y * blockDim.y + threadIdx.y;
	if (x >= dst_w || y >= dst_h)
		return;

	unsigned int x0 = x * src_w / dst_w;
	unsigned int x1 = (x + 1) * src_w / dst_w;
	if (x1 <= x0)
		x1 = x0 + 1;
	unsigned int y0 = y * src_h / dst_h;
	unsigned int y1 = (y + 1) * src_h / dst_h;
	if (y1 <= y0)
		y1 = y0 + 1;

	unsigned int sum_u = 0;
	unsigned int sum_v = 0;
	for (unsigned int sy = y0; sy < y1; sy++)
		for (unsigned int sx = x0; sx < x1; sx++) {
			sum_u += src[sy * src_pitch + 2 * sx];
			sum_v += src[sy * src_pitch + 2 * sx + 1];
		}

	unsigned int n = (x1 - x0) * (y1 - y0);
	dst[y * dst_pitch + 2 * x] = (unsigned char)((sum_u + n / 2) / n);
	dst[y * dst_pitch + 2 * x + 1] = (unsigned char)((sum_v + n / 2) / n);
}

// Packed 8-bit RGBA / BGRA to NV12, the GPU color conversion behind
// cuda::Converter. The source is a CUDA surface over the Vulkan image that
// frame/vulkan.rs imported, read in place; the destination is a pitched NV12
// allocation NVENC registers directly, so a rendered frame reaches the encoder
// without a staging copy.
//
// One thread per 2x2 block of source pixels: it writes the four luma samples
// and the one interleaved UV pair below them, so 4:2:0 chroma is the average of
// its block (sited at the block center). The matrix and range arrive as
// weights (see Color::coefficients in color.rs), which keeps one kernel for
// BT.601 / BT.709 and limited / full. The samples are display-referred 8-bit as
// the producer wrote them; no transfer function is applied, so a producer's
// final LDR output is not gamma-encoded twice.
//
// Vendored PTX: this file is compiled offline to rgba_to_nv12.ptx (see the
// comment there for the exact command), which is embedded and JIT-compiled by
// the driver at runtime. Building the crate needs no CUDA toolkit, matching
// the dlopen-only design of the NVENC/NVDEC backends. If you edit this file,
// regenerate the PTX next to it.

#include <cuda_runtime.h>

__device__ static unsigned char weigh(const float4 w, float r, float g, float b)
{
	float v = w.x * r + w.y * g + w.z * b + w.w;
	v = fminf(fmaxf(v, 0.0f), 255.0f);
	return (unsigned char)__float2int_rn(v);
}

extern "C" __global__ void rgba_to_nv12(
	cudaSurfaceObject_t src, unsigned int width, unsigned int height, unsigned int bgra,
	float4 luma, float4 cb, float4 cr,
	unsigned char *dst, unsigned int pitch)
{
	unsigned int x = 2 * (blockIdx.x * blockDim.x + threadIdx.x);
	unsigned int y = 2 * (blockIdx.y * blockDim.y + threadIdx.y);
	if (x >= width || y >= height)
		return;

	float r_sum = 0.0f, g_sum = 0.0f, b_sum = 0.0f;
	unsigned char *y_out = dst + y * pitch + x;
	for (unsigned int dy = 0; dy < 2; dy++) {
		for (unsigned int dx = 0; dx < 2; dx++) {
			// Surface reads are byte-addressed in x.
			uchar4 p = surf2Dread<uchar4>(src, (x + dx) * 4, y + dy);
			float r = bgra ? p.z : p.x;
			float g = p.y;
			float b = bgra ? p.x : p.z;
			r_sum += r;
			g_sum += g;
			b_sum += b;
			y_out[dy * pitch + dx] = weigh(luma, r, g, b);
		}
	}

	// The chroma plane starts after `height` luma rows and shares the pitch;
	// each row holds interleaved U, V pairs.
	unsigned char *uv_out = dst + height * pitch + (y / 2) * pitch + x;
	float r = r_sum * 0.25f, g = g_sum * 0.25f, b = b_sum * 0.25f;
	uv_out[0] = weigh(cb, r, g, b);
	uv_out[1] = weigh(cr, r, g, b);
}

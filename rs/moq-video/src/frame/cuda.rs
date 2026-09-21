//! Linux CUDA device memory: the NV12 [`Frame`] behind `Surface::Cuda`, which
//! NVDEC produces and NVENC consumes in place.

use std::sync::{Arc, OnceLock};

use cudarc::driver::{CudaContext, CudaFunction, LaunchConfig, PushKernelArg, result};

use super::I420;
use crate::Error;

/// The NV12 box-filter resize kernels, vendored as PTX (see nv12_resize.cu)
/// and JIT-compiled by the driver, so building needs no CUDA toolkit.
const RESIZE_PTX: &str = include_str!("nv12_resize.ptx");

/// The loaded resize kernels, one per process (everything runs in the
/// device's primary context, so one module serves every frame).
struct Kernels {
	luma: CudaFunction,
	chroma: CudaFunction,
}

fn kernels(ctx: &Arc<CudaContext>) -> Result<&'static Kernels, Error> {
	static KERNELS: OnceLock<Result<Kernels, String>> = OnceLock::new();
	KERNELS
		.get_or_init(|| {
			let module = ctx
				.load_module(cudarc::nvrtc::Ptx::from_src(RESIZE_PTX))
				.map_err(|e| format!("load nv12_resize PTX: {e:?}"))?;
			Ok(Kernels {
				luma: module
					.load_function("resize_luma")
					.map_err(|e| format!("load resize_luma: {e:?}"))?,
				chroma: module
					.load_function("resize_chroma")
					.map_err(|e| format!("load resize_chroma: {e:?}"))?,
			})
		})
		.as_ref()
		.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA resize unavailable: {e}")))
}

/// An owned device allocation. Plain `cuMemAlloc` on purpose: NVENC's
/// resource registration rejects stream-ordered pool memory
/// (`cuMemAllocAsync`), which is what cudarc's `CudaSlice` uses on any GPU
/// with memory-pool support.
struct Buffer {
	ctx: Arc<CudaContext>,
	ptr: cudarc::driver::sys::CUdeviceptr,
	len: usize,
}

impl Drop for Buffer {
	fn drop(&mut self) {
		// Drop may run on any thread; freeing needs the context current.
		if self.ctx.bind_to_thread().is_ok() {
			// SAFETY: the pointer came from `malloc_sync` and is freed once.
			let _ = unsafe { result::free_sync(self.ptr) };
		}
	}
}

/// A GPU NV12 frame in CUDA device memory: NVDEC's output and NVENC's
/// zero-copy input. One buffer holds both planes at a shared row `pitch`:
/// `height` luma rows, then `height / 2` interleaved-UV rows. Cloning bumps
/// refcounts (no pixel copy), which keeps decode -> encode on the GPU.
///
/// Both codecs use the device's primary CUDA context (`CudaContext::new`
/// retains it), so a frame decoded by NVDEC is directly addressable by NVENC.
#[derive(Clone)]
pub struct Frame {
	buf: Arc<Buffer>,
	pub(crate) width: u32,
	pub(crate) height: u32,
	/// Row pitch in bytes of both planes (>= `width`).
	pub(crate) pitch: u32,
}

impl Frame {
	/// Allocate an NV12 buffer for `width` x `height` (both even) at row
	/// pitch `pitch`. Uninitialized: the caller copies the full extent in.
	pub(crate) fn alloc(ctx: &Arc<CudaContext>, width: u32, height: u32, pitch: u32) -> Result<Self, Error> {
		debug_assert!(pitch >= width && width.is_multiple_of(2) && height.is_multiple_of(2));
		let len = pitch as usize * height as usize * 3 / 2;
		ctx.bind_to_thread()
			.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA bind: {e:?}")))?;
		// SAFETY: a plain device allocation; ownership lands in `Buffer`,
		// whose Drop frees it exactly once.
		let ptr = unsafe { result::malloc_sync(len) }
			.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA alloc of {len} bytes: {e:?}")))?;
		Ok(Self {
			buf: Arc::new(Buffer {
				ctx: ctx.clone(),
				ptr,
				len,
			}),
			width,
			height,
			pitch,
		})
	}

	/// The raw device pointer, for FFI (the NVDEC copy destination, the
	/// NVENC resource registration). Valid while `self` is alive.
	pub(crate) fn device_ptr(&self) -> u64 {
		self.buf.ptr
	}

	/// Download and de-pitch to packed I420 (the CPU fallback: a software
	/// encoder, or a caller that wants bytes).
	pub(crate) fn download_i420(&self) -> Result<I420, Error> {
		self.buf
			.ctx
			.bind_to_thread()
			.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA bind: {e:?}")))?;
		let mut host = vec![0u8; self.buf.len];
		// SAFETY: the buffer is `len` bytes of device memory and stays alive
		// for the synchronous copy.
		unsafe { result::memcpy_dtoh_sync(&mut host, self.buf.ptr) }
			.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA download: {e:?}")))?;

		let (w, h) = (self.width as usize, self.height as usize);
		let (cw, ch) = (w / 2, h / 2);
		let pitch = self.pitch as usize;

		let mut data = vec![0u8; I420::len(self.width, self.height)];
		let (luma, chroma) = data.split_at_mut(w * h);
		let (u_dst, v_dst) = chroma.split_at_mut(cw * ch);

		for row in 0..h {
			luma[row * w..row * w + w].copy_from_slice(&host[row * pitch..row * pitch + w]);
		}
		let uv_base = pitch * h;
		for row in 0..ch {
			let src = &host[uv_base + row * pitch..uv_base + row * pitch + w];
			for col in 0..cw {
				u_dst[row * cw + col] = src[col * 2];
				v_dst[row * cw + col] = src[col * 2 + 1];
			}
		}

		Ok(I420 {
			width: self.width,
			height: self.height,
			data,
			// A deinterleave, not a color conversion, and nothing here names
			// the space these samples are in. Left unknown to be inferred.
			color: None,
		})
	}

	/// Resize to `width` x `height` (both even) with the box-filter kernel,
	/// staying in device memory. The GPU half of
	/// [`Frame::resize`].
	pub(crate) fn resize(&self, width: u32, height: u32) -> Result<Self, Error> {
		let ctx = &self.buf.ctx;
		let kernels = kernels(ctx)?;

		// Destination row pitch aligned to 256 bytes: comfortable coalescing
		// and a multiple of 4 as NVENC registration requires.
		let pitch = width.next_multiple_of(256);
		let dst = Self::alloc(ctx, width, height, pitch)?;

		let stream = ctx.default_stream();
		let block = (16u32, 16, 1);
		let grid = |w: u32, h: u32| (w.div_ceil(16), h.div_ceil(16), 1);
		let launch_err = |plane: &str, e| Error::Codec(anyhow::anyhow!("CUDA resize {plane}: {e:?}"));

		// Luma plane: one thread per destination pixel.
		//
		// SAFETY: both buffers are live NV12 allocations of pitch * height *
		// 3 / 2 bytes, and the kernels bound every access by the dimensions
		// passed alongside the pointers.
		unsafe {
			stream
				.launch_builder(&kernels.luma)
				.arg(&self.buf.ptr)
				.arg(&self.pitch)
				.arg(&self.width)
				.arg(&self.height)
				.arg(&dst.buf.ptr)
				.arg(&pitch)
				.arg(&width)
				.arg(&height)
				.launch(LaunchConfig {
					grid_dim: grid(width, height),
					block_dim: block,
					shared_mem_bytes: 0,
				})
		}
		.map_err(|e| launch_err("luma", e))?;

		// Chroma plane: one thread per destination UV pair, offset past the
		// luma rows in both buffers.
		let src_uv = self.buf.ptr + u64::from(self.pitch) * u64::from(self.height);
		let dst_uv = dst.buf.ptr + u64::from(pitch) * u64::from(height);
		let (src_pw, src_ph) = (self.width / 2, self.height / 2);
		let (dst_pw, dst_ph) = (width / 2, height / 2);
		// SAFETY: as above; the UV offsets stay inside the same allocations.
		unsafe {
			stream
				.launch_builder(&kernels.chroma)
				.arg(&src_uv)
				.arg(&self.pitch)
				.arg(&src_pw)
				.arg(&src_ph)
				.arg(&dst_uv)
				.arg(&pitch)
				.arg(&dst_pw)
				.arg(&dst_ph)
				.launch(LaunchConfig {
					grid_dim: grid(dst_pw, dst_ph),
					block_dim: block,
					shared_mem_bytes: 0,
				})
		}
		.map_err(|e| launch_err("chroma", e))?;

		// The frame may head straight to NVENC (which does not order against
		// our stream), so wait for the kernels rather than queueing.
		stream
			.synchronize()
			.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA resize sync: {e:?}")))?;
		Ok(dst)
	}
}

//! Linux CUDA device memory: the NV12 [`Frame`] behind `Surface::Cuda`, which
//! NVDEC or a [`Converter`] produces and NVENC consumes in place.
//!
//! # The GPU-only path
//!
//! A producer that must keep raw pixels off the CPU (a game engine publishing
//! its render target) stays inside this module and `frame::vulkan`: import the
//! image ([`vulkan::Importer`]), convert it
//! ([`Converter::convert`]), scale it ([`Frame::resize`]), and encode it with an
//! [`encode::Encoder`](crate::encode::Encoder) opened as
//! `Kind::Named("nvenc")`, which registers a `Surface::Cuda` with NVENC in
//! place. Every operation on that path runs on the device or fails: nothing
//! here downloads, uploads, or converts on the CPU, and a request the device
//! cannot serve (a full pool, a mismatched device, an odd size) is an error
//! rather than a fallback.
//!
//! The portable `Surface` methods are a different contract. `Surface::resize`
//! keeps a stream alive by downloading when the GPU scaler fails, and a
//! software encoder reads a `Surface::Cuda` back through `Surface::to_i420`;
//! both exist for the transcode path, whose frames come from NVDEC and may go
//! anywhere. A GPU-only producer never calls them and never opens an encoder
//! with `Kind::Auto`, whose openh264 fallback would read the frame back.
//!
//! NVENC takes the converted frame as a registered `cuMemAlloc` NV12 buffer
//! (`NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR`), the registration the NVDEC
//! path already proved on hardware. The SDK 12.1 bindings could also register
//! the imported Vulkan image itself as a CUDA array in an RGB session, but NVENC
//! would then own the color matrix, and the smaller rendition would still need
//! its own conversion, so a kernel that writes NV12 straight from the image is
//! the one pass this path needs: no staging copy in either direction.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};

use cudarc::driver::{CudaContext, CudaFunction, LaunchConfig, PushKernelArg, result};

use super::pool::{self, Pool};
use super::{I420, vulkan};
use crate::{Color, Error, Size};

/// The NV12 box-filter resize kernels, vendored as PTX (see nv12_resize.cu)
/// and JIT-compiled by the driver, so building needs no CUDA toolkit.
const RESIZE_PTX: &str = include_str!("nv12_resize.ptx");

/// The loaded resize kernels, one set per device: a module belongs to the
/// context that loaded it, and every frame on a device shares that device's
/// primary context.
struct Kernels {
	luma: CudaFunction,
	chroma: CudaFunction,
}

/// Resize kernels by device ordinal, or why loading them failed there.
type Loaded = HashMap<usize, Result<Arc<Kernels>, String>>;

fn kernels(ctx: &Arc<CudaContext>) -> Result<Arc<Kernels>, Error> {
	static KERNELS: OnceLock<Mutex<Loaded>> = OnceLock::new();
	let mut loaded = KERNELS
		.get_or_init(Default::default)
		.lock()
		.expect("CUDA resize kernels poisoned");
	loaded
		.entry(ctx.ordinal())
		.or_insert_with(|| {
			let module = ctx
				.load_module(cudarc::nvrtc::Ptx::from_src(RESIZE_PTX))
				.map_err(|e| format!("load nv12_resize PTX: {e:?}"))?;
			Ok(Arc::new(Kernels {
				luma: module
					.load_function("resize_luma")
					.map_err(|e| format!("load resize_luma: {e:?}"))?,
				chroma: module
					.load_function("resize_chroma")
					.map_err(|e| format!("load resize_chroma: {e:?}"))?,
			}))
		})
		.clone()
		.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA resize unavailable: {e}")))
}

/// An owned device allocation. Plain `cuMemAlloc` on purpose: NVENC's
/// resource registration rejects stream-ordered pool memory
/// (`cuMemAllocAsync`), which is what cudarc's `CudaSlice` uses on any GPU
/// with memory-pool support.
struct Raw {
	ctx: Arc<CudaContext>,
	ptr: cudarc::driver::sys::CUdeviceptr,
	len: usize,
}

impl Raw {
	fn alloc(ctx: &Arc<CudaContext>, len: usize) -> Result<Self, Error> {
		ctx.bind_to_thread()
			.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA bind: {e:?}")))?;
		// SAFETY: a plain device allocation, freed exactly once by Drop.
		let ptr = unsafe { result::malloc_sync(len) }
			.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA alloc of {len} bytes: {e:?}")))?;
		Ok(Self {
			ctx: ctx.clone(),
			ptr,
			len,
		})
	}
}

impl Drop for Raw {
	fn drop(&mut self) {
		// Drop may run on any thread; freeing needs the context current.
		if self.ctx.bind_to_thread().is_ok() {
			// SAFETY: the pointer came from `malloc_sync` and is freed once.
			let _ = unsafe { result::free_sync(self.ptr) };
		}
	}
}

/// The pool's allocator: one context, plain device memory.
struct Device(Arc<CudaContext>);

impl pool::Alloc for Device {
	type Buffer = Raw;

	fn alloc(&self, len: usize) -> Result<Raw, Error> {
		Raw::alloc(&self.0, len)
	}
}

/// A frame's allocation: freed on drop, or handed back to the pool it came
/// from so the next frame reuses it.
struct Buffer {
	raw: std::mem::ManuallyDrop<Raw>,
	pool: Option<Arc<Pool<Device>>>,
}

impl Buffer {
	fn take(pool: &Arc<Pool<Device>>, len: usize) -> Result<Self, Error> {
		Ok(Self {
			raw: std::mem::ManuallyDrop::new(pool.take(len)?),
			pool: Some(pool.clone()),
		})
	}
}

impl std::ops::Deref for Buffer {
	type Target = Raw;

	fn deref(&self) -> &Raw {
		&self.raw
	}
}

impl Drop for Buffer {
	fn drop(&mut self) {
		// SAFETY: taken exactly once, here; nothing reads `raw` afterwards.
		let raw = unsafe { std::mem::ManuallyDrop::take(&mut self.raw) };
		match &self.pool {
			Some(pool) => pool.put(raw.len, raw),
			None => drop(raw),
		}
	}
}

/// A GPU NV12 frame in CUDA device memory: NVDEC's or a [`Converter`]'s
/// output and NVENC's zero-copy input. One buffer holds both planes at a
/// shared row `pitch`: `height` luma rows, then `height / 2` interleaved-UV
/// rows. Cloning bumps refcounts (no pixel copy), which keeps decode -> encode
/// on the GPU; an encoder holds a clone until it has read the frame.
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
	/// The space the samples are in, when the crate converted them itself.
	color: Option<Color>,
}

impl std::fmt::Debug for Frame {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Frame")
			.field("size", &self.size())
			.field("pitch", &self.pitch)
			.field("color", &self.color)
			.field("pooled", &self.buf.pool.is_some())
			.finish()
	}
}

/// NV12 bytes for `height` rows at `pitch`: the luma rows plus half as many
/// chroma rows. Refused rather than wrapped when the product does not fit, so
/// an absurd size cannot become a short allocation the kernels overrun.
fn nv12_len(height: u32, pitch: u32) -> Result<usize, Error> {
	(pitch as usize)
		.checked_mul(height as usize)
		.and_then(|luma| luma.checked_mul(3))
		.map(|bytes| bytes / 2)
		.ok_or_else(|| {
			Error::Codec(anyhow::anyhow!(
				"NV12 frame of {height} rows at pitch {pitch} is too large"
			))
		})
}

/// The row pitch for frames this module allocates: 256-byte aligned for
/// comfortable coalescing, and a multiple of 4 as NVENC registration requires.
/// A width whose pitch does not fit `u32` is refused rather than wrapped into
/// an allocation the kernels would overrun.
fn aligned_pitch(width: u32) -> Result<u32, Error> {
	width
		.checked_next_multiple_of(256)
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("frame width {width} is too wide for a CUDA NV12 pitch")))
}

impl Frame {
	/// Allocate an NV12 buffer for `width` x `height` (both even) at row
	/// pitch `pitch`. Uninitialized: the caller copies the full extent in.
	pub(crate) fn alloc(ctx: &Arc<CudaContext>, width: u32, height: u32, pitch: u32) -> Result<Self, Error> {
		debug_assert!(pitch >= width && width.is_multiple_of(2) && height.is_multiple_of(2));
		let raw = Raw::alloc(ctx, nv12_len(height, pitch)?)?;
		Ok(Self {
			buf: Arc::new(Buffer {
				raw: std::mem::ManuallyDrop::new(raw),
				pool: None,
			}),
			width,
			height,
			pitch,
			color: None,
		})
	}

	/// An uninitialized frame from `pool` at the aligned pitch.
	fn pooled(pool: &Arc<Pool<Device>>, size: Size, color: Option<Color>) -> Result<Self, Error> {
		let pitch = aligned_pitch(size.width)?;
		Ok(Self {
			buf: Arc::new(Buffer::take(pool, nv12_len(size.height, pitch)?)?),
			width: size.width,
			height: size.height,
			pitch,
			color,
		})
	}

	/// The frame size in pixels.
	pub fn size(&self) -> Size {
		Size::new(self.width, self.height)
	}

	/// The color space of the samples: the one a [`Converter`] wrote, or `None`
	/// for a decoded frame that merely passed through.
	pub fn color(&self) -> Option<Color> {
		self.color
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

		let mut data = vec![0u8; I420::len(self.size())?];
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
			// A deinterleave, not a color conversion: the space is whatever the
			// samples were in, known only when the crate converted them.
			color: self.color,
		})
	}

	/// A copy scaled to `size` (both dimensions even) with the box-filter
	/// kernel, staying in device memory and in the same color space.
	///
	/// GPU only: a kernel the driver refuses is an error here. The CPU fallback
	/// belongs to [`Surface::resize`](crate::Surface::resize), which calls this
	/// first. A frame from a [`Converter`] draws the copy from the same bounded
	/// pool, so scaling one captured frame to every rendition still holds a
	/// fixed number of buffers.
	pub fn resize(&self, size: Size) -> Result<Self, Error> {
		size.validate("resize to")?;
		let Size { width, height } = size;
		let ctx = &self.buf.ctx;
		let kernels = kernels(ctx)?;

		let dst = match &self.buf.pool {
			Some(pool) => Self::pooled(pool, size, self.color)?,
			None => {
				let mut dst = Self::alloc(ctx, width, height, aligned_pitch(width)?)?;
				dst.color = self.color;
				dst
			}
		};
		let pitch = dst.pitch;

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

/// The RGBA / BGRA to NV12 kernel, vendored as PTX (see rgba_to_nv12.cu) and
/// JIT-compiled by the driver like the resize kernels.
const CONVERT_PTX: &str = include_str!("rgba_to_nv12.ptx");

/// Converts imported Vulkan images to NV12 [`Frame`]s on the GPU, from a
/// bounded pool of device buffers.
///
/// One converter serves one device and one color space: the matrix and range
/// it writes are what the encoder opened for the same
/// [`encode::Config::color`](crate::encode::Config::color) declares in the
/// bitstream, and the frames it returns report that space through
/// [`Frame::color`] so the two cannot silently disagree. Convert once at the
/// captured size and [`Frame::resize`] the result for smaller renditions; every
/// buffer, converted or scaled, comes from the pool sized by `capacity`, so a
/// producer that outruns its encoder gets an error from
/// [`convert`](Self::convert) instead of unbounded device memory. Drop the
/// frames the encoder has finished with (it retains its own clone until then)
/// and the buffers come back.
///
/// The pixels are taken as the producer's final display-referred output: no
/// transfer function is applied on the way to Y'CbCr, so an sRGB-encoded 8-bit
/// render target is not gamma-encoded twice. Chroma is 4:2:0, each sample the
/// average of its 2x2 block.
pub struct Converter {
	ctx: Arc<CudaContext>,
	color: Color,
	kernel: CudaFunction,
	pool: Arc<Pool<Device>>,
}

impl std::fmt::Debug for Converter {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Converter")
			.field("device", &self.ctx.ordinal())
			.field("color", &self.color)
			.field("capacity", &self.pool.capacity())
			.finish_non_exhaustive()
	}
}

impl Converter {
	/// Open CUDA device `ordinal`, load the conversion kernel, and size the
	/// buffer pool. Two live frames per captured frame (the converted one and
	/// one scaled copy) plus one of slack is a reasonable `capacity`.
	pub fn new(ordinal: usize, color: Color, capacity: NonZeroUsize) -> Result<Self, Error> {
		let ctx = CudaContext::new(ordinal)
			.map_err(|e| Error::Unsupported(format!("CUDA device {ordinal} is unavailable: {e:?}")))?;
		let kernel = ctx
			.load_module(cudarc::nvrtc::Ptx::from_src(CONVERT_PTX))
			.and_then(|module| module.load_function("rgba_to_nv12"))
			.map_err(|e| Error::Unsupported(format!("CUDA color conversion unavailable: {e:?}")))?;
		Ok(Self {
			pool: Arc::new(Pool::new(Device(ctx.clone()), capacity)),
			ctx,
			color,
			kernel,
		})
	}

	/// The color space every converted frame is in.
	pub fn color(&self) -> Color {
		self.color
	}

	/// Convert a published Vulkan image to an NV12 frame at the same size.
	///
	/// Reads the image in place on its own stream, behind the producer's
	/// ready signal, and returns once the kernel has finished: the image may
	/// be dropped afterwards, and the frame can go straight to an encoder that
	/// does not order against CUDA streams. Fails without touching the CPU when
	/// the image lives on another device, has an odd dimension (4:2:0 chroma
	/// needs even ones), or the pool has no buffer free.
	pub fn convert(&self, frame: &vulkan::Frame) -> Result<Frame, Error> {
		if frame.cuda_context().ordinal() != self.ctx.ordinal() {
			return Err(Error::Unsupported(format!(
				"Vulkan image on CUDA device {} cannot be converted on device {}",
				frame.cuda_context().ordinal(),
				self.ctx.ordinal()
			)));
		}
		let size = frame.size();
		size.validate("Vulkan/CUDA conversion of")?;

		let dst = Frame::pooled(&self.pool, size, Some(self.color))?;
		let weights = self.color.coefficients();
		let bgra = u32::from(frame.channels() == vulkan::Channels::Bgra);
		let stream = frame.cuda_stream();

		// One thread per 2x2 block: the kernel writes four luma samples and one
		// UV pair.
		let (blocks_w, blocks_h) = (size.width / 2, size.height / 2);
		// SAFETY: the surface object is live for as long as `frame` is, the
		// destination is a live NV12 allocation of pitch * height * 3 / 2
		// bytes, and the kernel bounds every access by the dimensions passed
		// alongside.
		unsafe {
			stream
				.launch_builder(&self.kernel)
				.arg(&frame.cuda_surface())
				.arg(&size.width)
				.arg(&size.height)
				.arg(&bgra)
				.arg(&weights.y)
				.arg(&weights.u)
				.arg(&weights.v)
				.arg(&dst.buf.ptr)
				.arg(&dst.pitch)
				.launch(LaunchConfig {
					grid_dim: (blocks_w.div_ceil(16), blocks_h.div_ceil(16), 1),
					block_dim: (16, 16, 1),
					shared_mem_bytes: 0,
				})
		}
		.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA color conversion: {e:?}")))?;

		// The frame may head straight to NVENC, which does not order against
		// our stream, and the caller may drop the image next; wait here.
		stream
			.synchronize()
			.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA color conversion sync: {e:?}")))?;
		Ok(dst)
	}
}

#[cfg(test)]
#[path = "cuda_test.rs"]
mod tests;

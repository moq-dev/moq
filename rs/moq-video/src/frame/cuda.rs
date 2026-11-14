//! Linux CUDA device memory: the NV12 [`Frame`] behind `Surface::Cuda`, which
//! NVDEC or a `Converter` produces and NVENC scales into its own input on the
//! GPU.
//!
//! # The GPU-only path
//!
//! External producers publish `Surface::Vulkan` with `encode::Config::input`
//! naming their device. NVENC imports and converts those images here, caching
//! one full-size conversion per capture and color space, then scaling each
//! rendition on the GPU. Those images are never downloaded.
//!
//! Native `Surface::Cuda` frames from NVDEC also stay on the GPU. Their portable
//! `Surface` methods permit CPU readback for software transcoding, while an
//! external Vulkan image refuses that fallback.
//!
//! NVENC reads a registered `cuMemAlloc` NV12 buffer
//! (`NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR`): each session's own `Target`,
//! which every frame is copied into, or scaled into when its size differs. The
//! one copy is what lets a session register once rather than per frame, and
//! costs microseconds of GPU time. The SDK 12.1 bindings could also register
//! the imported Vulkan image itself as a CUDA array in an RGB session, but NVENC
//! would then own the color matrix, and the smaller rendition would still need
//! its own conversion, so a kernel that writes NV12 straight from the image is
//! the one pass this path needs: no staging copy in either direction.
//!
//! What the hardware says (RTX 3070 Ti, driver 595.91.07, `just rs gpu`):
//! the kernels match a CPU reference exactly, and an `nsys --trace=cuda` capture
//! of `tests::vulkan_cuda_three_view_workload` records no `cuMemcpy` in either
//! direction.
//!
//! # Nothing per frame takes the driver's device lock
//!
//! Allocating, freeing, and registering device memory go through the
//! driver's per-GPU lock, which every process on the device shares, and
//! `cuMemFree` also waits for the whole device. Paid per frame, those calls
//! serialize every transcode on the GPU while its engines sit idle. So every
//! frame's buffer comes from a `Pool` and goes back to it, and each NVENC
//! session copies or scales its input into one `Target` it registered once. Scaling
//! and copies run on their owner's own stream, never the legacy default
//! stream, which would wait for every other thread's work in the process.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};

use cudarc::driver::{CudaContext, CudaFunction, CudaStream, LaunchConfig, PushKernelArg, result};

use super::pool::{self, Pool};
use super::{I420, cuda_vulkan, vulkan};
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
		.map_err(|e| Error::codec(anyhow::anyhow!("CUDA resize unavailable: {e}")))
}

/// Whether the driver loads the resize kernels on `ctx`'s device. A driver can
/// refuse the vendored PTX, so a consumer that would scale on the GPU checks
/// once and otherwise leaves scaling to [`Surface::resize`](crate::Surface::resize),
/// which falls back to the CPU.
pub(crate) fn scales(ctx: &Arc<CudaContext>) -> bool {
	kernels(ctx).is_ok()
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
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA bind: {e:?}")))?;
		// SAFETY: a plain device allocation, freed exactly once by Drop.
		let ptr = unsafe { result::malloc_sync(len) }
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA alloc of {len} bytes: {e:?}")))?;
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

/// Recycled NV12 buffers on one device for a producer that cannot refuse a
/// frame, such as a decoder: never exhausted, and holding at most as many
/// buffers as its frames ever had alive at once. Frames scaled from these draw
/// from the same buffers.
#[derive(Clone)]
pub(crate) struct Frames(Arc<Pool<Device>>);

impl Frames {
	/// Buffers on `ctx`'s device.
	pub(crate) fn new(ctx: &Arc<CudaContext>) -> Self {
		Self(Arc::new(Pool::new(Device(ctx.clone()), None)))
	}

	/// An uninitialized frame of `size` (both dimensions even) at row pitch
	/// `pitch`: the caller writes the full extent.
	pub(crate) fn alloc(&self, size: Size, pitch: u32) -> Result<Frame, Error> {
		let reservation = self.0.reserve().expect("an unbounded pool always has room");
		Frame::pooled(reservation, size, pitch, None)
	}
}

/// A GPU NV12 frame in CUDA device memory: NVDEC's or a `Converter`'s
/// output and NVENC's zero-copy input. One buffer holds both planes at a
/// shared row `pitch`: `height` luma rows, then `height / 2` interleaved-UV
/// rows. Cloning bumps refcounts (no pixel copy), which keeps decode -> encode
/// on the GPU; an encoder holds a clone until it has read the frame.
///
/// Both codecs use the device's primary CUDA context (`CudaContext::new`
/// retains it), so a frame decoded by NVDEC is directly addressable by NVENC.
#[derive(Clone)]
pub struct Frame {
	buf: Arc<pool::Lease<Device>>,
	pub(crate) width: u32,
	pub(crate) height: u32,
	/// Row pitch in bytes of both planes (>= `width`).
	pub(crate) pitch: u32,
	/// The space the samples are in: the one a `Converter` wrote, or the stream's
	/// when a decoder produced them.
	pub(crate) color: Option<Color>,
}

impl std::fmt::Debug for Frame {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Frame")
			.field("size", &self.size())
			.field("pitch", &self.pitch)
			.field("color", &self.color)
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
			Error::codec(anyhow::anyhow!(
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
		.ok_or_else(|| Error::codec(anyhow::anyhow!("frame width {width} is too wide for a CUDA NV12 pitch")))
}

impl Frame {
	/// An uninitialized frame of `size` filling `reservation` at row pitch `pitch`.
	fn pooled(
		reservation: pool::Reservation<Device>,
		size: Size,
		pitch: u32,
		color: Option<Color>,
	) -> Result<Self, Error> {
		debug_assert!(pitch >= size.width && size.width.is_multiple_of(2) && size.height.is_multiple_of(2));
		Ok(Self {
			buf: Arc::new(reservation.fill(nv12_len(size.height, pitch)?)?),
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

	/// The color space of the samples: the one a `Converter` wrote, or `None`
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
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA bind: {e:?}")))?;
		let mut host = vec![0u8; self.buf.len];
		// SAFETY: the buffer is `len` bytes of device memory and stays alive
		// for the synchronous copy.
		unsafe { result::memcpy_dtoh_sync(&mut host, self.buf.ptr) }
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA download: {e:?}")))?;

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

	/// A copy scaled to `size` (both dimensions even) on the GPU, for
	/// [`Surface::resize`](crate::Surface::resize), which falls back to the CPU
	/// when this fails. The copy comes from this frame's own pool, so a full
	/// bounded pool is one such failure; an NVENC session scales into its own
	/// [`Target`] instead.
	pub(crate) fn resize(&self, size: Size) -> Result<Self, Error> {
		size.validate("resize to")?;
		let pool = self.buf.pool();
		let reservation = pool.reserve().ok_or_else(|| {
			Error::Unsupported(format!(
				"GPU frame pool of {} buffers exhausted",
				pool.capacity().map_or(0, NonZeroUsize::get)
			))
		})?;
		let dst = Self::pooled(reservation, size, aligned_pitch(size.width)?, self.color)?;
		self.scale_into(&dst, &self.buf.ctx.default_stream())?;
		Ok(dst)
	}

	/// Copy into `dst`, the same size, on `stream` and wait for it. The copy
	/// engine needs no PTX, unlike [`scale_into`](Self::scale_into).
	fn copy_into(&self, dst: &Self, stream: &Arc<CudaStream>) -> Result<(), Error> {
		stream
			.context()
			.bind_to_thread()
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA bind: {e:?}")))?;
		// NV12 keeps its chroma rows right after the luma rows at the same pitch,
		// so one copy of 3/2 the height moves both planes.
		let copy = cudarc::driver::sys::CUDA_MEMCPY2D {
			srcXInBytes: 0,
			srcY: 0,
			srcMemoryType: cudarc::driver::sys::CUmemorytype::CU_MEMORYTYPE_DEVICE,
			srcHost: std::ptr::null(),
			srcDevice: self.buf.ptr,
			srcArray: std::ptr::null_mut(),
			srcPitch: self.pitch as usize,
			dstXInBytes: 0,
			dstY: 0,
			dstMemoryType: cudarc::driver::sys::CUmemorytype::CU_MEMORYTYPE_DEVICE,
			dstHost: std::ptr::null_mut(),
			dstDevice: dst.buf.ptr,
			dstArray: std::ptr::null_mut(),
			dstPitch: dst.pitch as usize,
			WidthInBytes: self.width as usize,
			Height: self.height as usize * 3 / 2,
		};
		// SAFETY: both buffers are live NV12 allocations of `width` x `height` at
		// their own pitch, which is at least `width`, so every row stays inside.
		unsafe { cudarc::driver::sys::cuMemcpy2DAsync_v2(&copy, stream.cu_stream()) }
			.result()
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA copy: {e:?}")))?;
		// As for `scale_into`: NVENC orders only against the default stream.
		stream
			.synchronize()
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA copy sync: {e:?}")))
	}

	/// Scale this frame into `dst` with the box-filter kernels on `stream`, a
	/// stream of this frame's device, and wait for them.
	fn scale_into(&self, dst: &Self, stream: &Arc<CudaStream>) -> Result<(), Error> {
		let kernels = kernels(&self.buf.ctx)?;
		let (width, height) = (dst.width, dst.height);
		let pitch = dst.pitch;

		let block = (16u32, 16, 1);
		let grid = |w: u32, h: u32| (w.div_ceil(16), h.div_ceil(16), 1);
		let launch_err = |plane: &str, e| Error::codec(anyhow::anyhow!("CUDA resize {plane}: {e:?}"));

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

		// The frame may head straight to NVENC, which orders only against the
		// default stream, so wait for the kernels rather than queueing.
		stream
			.synchronize()
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA resize sync: {e:?}")))
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
/// captured size, into a `Slot` from `reserve`, and let each rendition's
/// encoder scale the result into its own [`Target`]. Every converted buffer
/// comes from the pool sized by `capacity`, so a producer that outruns its
/// encoder gets `None` from `reserve` instead of unbounded device memory. Drop
/// the frames the encoders have finished with and the buffers come back.
///
/// The pixels are taken as the producer's final display-referred output: no
/// transfer function is applied on the way to Y'CbCr, so an sRGB-encoded 8-bit
/// render target is not gamma-encoded twice. Chroma is 4:2:0, each sample the
/// average of its 2x2 block.
pub(crate) struct Converter {
	ctx: Arc<CudaContext>,
	color: Color,
	kernel: CudaFunction,
	pool: Arc<Pool<Device>>,
	importer: cuda_vulkan::Importer,
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
	/// buffer pool. One live frame per captured frame in flight, plus slack, is
	/// a reasonable `capacity`.
	pub(crate) fn new(ordinal: usize, color: Color, capacity: NonZeroUsize) -> Result<Self, Error> {
		let ctx = CudaContext::new(ordinal)
			.map_err(|e| Error::Unsupported(format!("CUDA device {ordinal} is unavailable: {e:?}")))?;
		let kernel = ctx
			.load_module(cudarc::nvrtc::Ptx::from_src(CONVERT_PTX))
			.and_then(|module| module.load_function("rgba_to_nv12"))
			.map_err(|e| Error::Unsupported(format!("CUDA color conversion unavailable: {e:?}")))?;
		Ok(Self {
			importer: cuda_vulkan::Importer::new(ordinal)?,
			pool: Arc::new(Pool::new(Device(ctx.clone()), Some(capacity))),
			ctx,
			color,
			kernel,
		})
	}

	/// The color space every converted frame is in.
	pub fn color(&self) -> Color {
		self.color
	}

	/// Import `frame` as its reader, so dropping it returns the producer slot
	/// even when no pool buffer is free to convert it.
	pub(crate) fn import(&self, frame: &vulkan::Frame) -> Result<(), Error> {
		self.importer.import(frame).map(drop)
	}

	/// Hold one pool buffer for the next frame, or `None` while every buffer is
	/// live: back-pressure, so drop the frame and try again after the encoder
	/// releases one.
	pub(crate) fn reserve(&self) -> Option<Slot> {
		Some(Slot {
			reservation: self.pool.reserve()?,
			ctx: self.ctx.clone(),
			color: self.color,
			kernel: self.kernel.clone(),
			importer: self.importer.clone(),
		})
	}
}

/// One buffer held from a `Converter`'s pool, filled by converting or
/// scaling a frame into it. Dropped unfilled, or on a failed fill, it goes
/// back to the pool.
pub(crate) struct Slot {
	reservation: pool::Reservation<Device>,
	ctx: Arc<CudaContext>,
	color: Color,
	kernel: CudaFunction,
	importer: cuda_vulkan::Importer,
}

impl std::fmt::Debug for Slot {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Slot")
			.field("device", &self.ctx.ordinal())
			.field("color", &self.color)
			.finish_non_exhaustive()
	}
}

impl Slot {
	/// Convert a published Vulkan image to an NV12 frame at the same size.
	///
	/// Reads the image in place on its own stream, behind the producer's
	/// ready signal, and returns once the kernel has finished: the image may
	/// be dropped afterwards, and the frame can go straight to an encoder that
	/// does not order against CUDA streams. Fails without touching the CPU when
	/// the image lives on another device or has an odd dimension (4:2:0 chroma
	/// needs even ones).
	pub(crate) fn convert(self, frame: &vulkan::Frame) -> Result<Frame, Error> {
		let frame = self.importer.import(frame)?;
		if frame.cuda_context().ordinal() != self.ctx.ordinal() {
			return Err(Error::Unsupported(format!(
				"Vulkan image on CUDA device {} cannot be converted on device {}",
				frame.cuda_context().ordinal(),
				self.ctx.ordinal()
			)));
		}
		let size = frame.size();
		size.validate("Vulkan/CUDA conversion of")?;

		let dst = Frame::pooled(self.reservation, size, aligned_pitch(size.width)?, Some(self.color))?;
		let weights = self.color.coefficients();
		let bgra = u32::from(frame.format() == vulkan::Format::Bgra8);
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
		.map_err(|e| Error::codec(anyhow::anyhow!("CUDA color conversion: {e:?}")))?;

		// The frame may head straight to NVENC, which does not order against
		// our stream, and the caller may drop the image next; wait here.
		stream
			.synchronize()
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA color conversion sync: {e:?}")))?;
		Ok(dst)
	}
}

/// One NV12 buffer that frames are copied or scaled into, for a consumer that reads it
/// in place: an NVENC session registers it once and encodes every frame from
/// it. Scaling runs on the target's own stream, so it never waits on other
/// work in the process.
pub(crate) struct Target {
	frame: Frame,
	stream: Arc<CudaStream>,
}

impl std::fmt::Debug for Target {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Target")
			.field("frame", &self.frame)
			.finish_non_exhaustive()
	}
}

impl Target {
	/// Allocate a `size` (both dimensions even) buffer on `ctx`'s device.
	pub(crate) fn new(ctx: &Arc<CudaContext>, size: Size) -> Result<Self, Error> {
		size.validate("CUDA target")?;
		let frame = Frames::new(ctx).alloc(size, aligned_pitch(size.width)?)?;
		let stream = ctx
			.new_stream()
			.map_err(|e| Error::codec(anyhow::anyhow!("CUDA stream: {e:?}")))?;
		Ok(Self { frame, stream })
	}

	/// The buffer, uninitialized until the first [`fill`](Self::fill).
	pub(crate) fn frame(&self) -> &Frame {
		&self.frame
	}

	/// Copy or scale `src` into the buffer and wait for it, so the consumer may
	/// read the buffer next. A copy needs no kernel; scaling fails without
	/// touching the CPU when the driver refuses the kernels (see [`scales`]).
	/// Either fails when `src` lives on another device.
	pub(crate) fn fill(&mut self, src: &Frame) -> Result<(), Error> {
		if src.buf.ctx.ordinal() != self.frame.buf.ctx.ordinal() {
			return Err(Error::Unsupported(format!(
				"CUDA frame on device {} cannot be scaled on device {}",
				src.buf.ctx.ordinal(),
				self.frame.buf.ctx.ordinal()
			)));
		}
		if src.size() == self.frame.size() {
			src.copy_into(&self.frame, &self.stream)?;
		} else {
			src.scale_into(&self.frame, &self.stream)?;
		}
		self.frame.color = src.color;
		Ok(())
	}
}

#[cfg(test)]
#[path = "cuda_test.rs"]
mod tests;

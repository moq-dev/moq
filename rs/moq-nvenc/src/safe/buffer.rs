//! Defines traits and types for dealing with input and output buffers.

use std::{ffi::c_void, mem::ManuallyDrop, ptr, sync::Arc};

use cudarc::driver::{DevicePtr, MappedBuffer};

use super::{
	encoder::Encoder,
	result::{EncodeError, ErrorKind},
	session::Session,
};
use crate::sys::nvEncodeAPI::{
	NV_ENC_BUFFER_FORMAT, NV_ENC_CREATE_BITSTREAM_BUFFER, NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
	NV_ENC_CREATE_INPUT_BUFFER, NV_ENC_CREATE_INPUT_BUFFER_VER, NV_ENC_INPUT_RESOURCE_TYPE, NV_ENC_LOCK_BITSTREAM,
	NV_ENC_LOCK_BITSTREAM_VER, NV_ENC_LOCK_INPUT_BUFFER, NV_ENC_LOCK_INPUT_BUFFER_VER, NV_ENC_MAP_INPUT_RESOURCE,
	NV_ENC_MAP_INPUT_RESOURCE_VER, NV_ENC_PIC_TYPE, NV_ENC_REGISTER_RESOURCE,
};

pub(crate) mod sealed {
	pub trait Input {
		/// Give up an input the driver may still be reading: leak its handle, but
		/// release its encoder reference so the session can still be destroyed.
		fn abandon(self);
	}
}

/// An input buffer created or registered by this crate.
///
/// This trait is sealed so safe callers cannot forge driver handles.
///
/// ```compile_fail
/// use std::sync::Arc;
/// use moq_nvenc::{Encoder, EncoderInput};
/// struct Forged;
/// impl EncoderInput for Forged {
///     fn pitch(&self) -> u32 { 0 }
///     fn handle(&mut self) -> *mut std::ffi::c_void { std::ptr::null_mut() }
///     fn encoder(&self) -> &Arc<Encoder> { unimplemented!() }
/// }
/// ```
pub trait EncoderInput: sealed::Input {
	/// Get the pitch (AKA stride) of the input resource.
	fn pitch(&self) -> u32;

	/// Get the handle of the input resource.
	fn handle(&mut self) -> *mut c_void;

	/// Get the encoder that owns this input.
	fn encoder(&self) -> &Arc<Encoder>;
}

/// The driver calls behind an external resource, injectable so rollback can be
/// tested without an NVIDIA driver.
trait ResourceApi {
	fn register_resource(&self, params: &mut NV_ENC_REGISTER_RESOURCE) -> Result<*mut c_void, EncodeError>;
	fn map_input_resource(&self, registered: *mut c_void) -> Result<*mut c_void, EncodeError>;
	fn unmap_input_resource(&self, mapped: *mut c_void) -> Result<(), EncodeError>;
	fn unregister_resource(&self, registered: *mut c_void) -> Result<(), EncodeError>;
}

impl ResourceApi for Arc<Encoder> {
	fn register_resource(&self, params: &mut NV_ENC_REGISTER_RESOURCE) -> Result<*mut c_void, EncodeError> {
		unsafe { (self.api.register_resource)(self.ptr, params) }.result(self)?;
		Ok(params.registeredResource)
	}

	fn map_input_resource(&self, registered: *mut c_void) -> Result<*mut c_void, EncodeError> {
		let mut params = NV_ENC_MAP_INPUT_RESOURCE {
			version: NV_ENC_MAP_INPUT_RESOURCE_VER,
			registeredResource: registered,
			mappedResource: ptr::null_mut(),
			mappedBufferFmt: NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_UNDEFINED,
			..Default::default()
		};
		unsafe { (self.api.map_input_resource)(self.ptr, &mut params) }.result(self)?;
		Ok(params.mappedResource)
	}

	fn unmap_input_resource(&self, mapped: *mut c_void) -> Result<(), EncodeError> {
		unsafe { (self.api.unmap_input_resource)(self.ptr, mapped) }.result(self)
	}

	fn unregister_resource(&self, registered: *mut c_void) -> Result<(), EncodeError> {
		unsafe { (self.api.unregister_resource)(self.ptr, registered) }.result(self)
	}
}

/// Functions for creating input and output buffers.
impl Session {
	/// Create a [`Buffer`].
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#creating-resources-required-to-hold-inputoutput-data).
	///
	/// # Errors
	///
	/// Could error if the `width`, `height`, or `buffer_format` is invalid,
	/// or if we run out of memory.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::{
	/// #     sys::nvEncodeAPI::{
	/// #         NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB,
	/// #         NV_ENC_CODEC_H264_GUID,
	/// #         NV_ENC_PIC_PARAMS,
	/// #         NV_ENC_PIC_STRUCT,
	/// #     },
	/// #     Encoder, EncoderInitParams
	/// # };
	/// # const WIDTH: u32 = 1920;
	/// # const HEIGHT: u32 = 1080;
	/// //* Create encoder. *//
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// # let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	///
	/// //* Set `encode_guid` and `buffer_format`, and check that H.264 encoding and the ARGB format are supported. *//
	/// # let encode_guid = NV_ENC_CODEC_H264_GUID;
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&encode_guid));
	/// # let buffer_format = NV_ENC_BUFFER_FORMAT_ARGB;
	/// # let input_formats = encoder.get_supported_input_formats(encode_guid).unwrap();
	/// # assert!(input_formats.contains(&buffer_format));
	///
	/// //* Begin encoder session. *//
	/// # let mut initialize_params = EncoderInitParams::new(encode_guid, WIDTH, HEIGHT);
	/// # initialize_params.display_aspect_ratio(16, 9)
	/// #     .framerate(30, 1)
	/// #     .enable_picture_type_decision();
	/// # let session = encoder.start_session(
	/// #     buffer_format,
	/// #     initialize_params,
	/// # ).unwrap();
	///
	/// // Create an input buffer.
	/// let _input_buffer = session
	///     .create_input_buffer()
	///     .unwrap();
	/// ```
	pub fn create_input_buffer(&self) -> Result<Buffer, EncodeError> {
		let mut create_input_buffer_params = NV_ENC_CREATE_INPUT_BUFFER {
			version: NV_ENC_CREATE_INPUT_BUFFER_VER,
			width: self.width,
			height: self.height,
			bufferFmt: self.buffer_format,
			inputBuffer: ptr::null_mut(),
			..Default::default()
		};
		unsafe { (self.encoder.api.create_input_buffer)(self.encoder.ptr, &mut create_input_buffer_params) }
			.result(&self.encoder)?;
		Ok(Buffer {
			ptr: create_input_buffer_params.inputBuffer,
			pitch: self.width,
			encoder: self.encoder.clone(),
		})
	}

	/// Create a [`Bitstream`].
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#creating-resources-required-to-hold-inputoutput-data).
	///
	/// # Errors
	///
	/// Could error is we run out of memory.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::{
	/// #     sys::nvEncodeAPI::{
	/// #         NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB,
	/// #         NV_ENC_CODEC_H264_GUID,
	/// #         NV_ENC_PIC_PARAMS,
	/// #         NV_ENC_PIC_STRUCT,
	/// #     },
	/// #     Encoder, EncoderInitParams
	/// # };
	/// # const WIDTH: u32 = 1920;
	/// # const HEIGHT: u32 = 1080;
	/// //* Create encoder. *//
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// # let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	///
	/// //* Set `encode_guid` and `buffer_format`, and check that H.264 encoding and the ARGB format are supported. *//
	/// # let encode_guid = NV_ENC_CODEC_H264_GUID;
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&encode_guid));
	/// # let buffer_format = NV_ENC_BUFFER_FORMAT_ARGB;
	/// # let input_formats = encoder.get_supported_input_formats(encode_guid).unwrap();
	/// # assert!(input_formats.contains(&buffer_format));
	///
	/// //* Begin encoder session. *//
	/// # let mut initialize_params = EncoderInitParams::new(encode_guid, WIDTH, HEIGHT);
	/// # initialize_params.display_aspect_ratio(16, 9)
	/// #     .framerate(30, 1)
	/// #     .enable_picture_type_decision();
	/// # let session = encoder.start_session(
	/// #     buffer_format,
	/// #     initialize_params,
	/// # ).unwrap();
	///
	/// // Create an output bitstream buffer.
	/// let _output_bitstream = session
	///     .create_output_bitstream()
	///     .unwrap();
	/// ```
	pub fn create_output_bitstream(&self) -> Result<Bitstream, EncodeError> {
		let mut create_bitstream_buffer_params = NV_ENC_CREATE_BITSTREAM_BUFFER {
			version: NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
			bitstreamBuffer: ptr::null_mut(),
			..Default::default()
		};
		unsafe { (self.encoder.api.create_bitstream_buffer)(self.encoder.ptr, &mut create_bitstream_buffer_params) }
			.result(&self.encoder)?;
		Ok(Bitstream {
			ptr: create_bitstream_buffer_params.bitstreamBuffer,
			encoder: self.encoder.clone(),
		})
	}

	/// Create a [`RegisteredResource`] from a [`MappedBuffer`].
	///
	/// See [`Session::register_generic_resource`].
	///
	/// `pitch` should be set to the value obtained from `cuMemAllocPitch()`,
	/// or to the width in **bytes** (if this resource was created by using
	/// `cuMemAlloc()`). This value must be a multiple of 4.
	///
	/// # Errors
	///
	/// Could error if registration fails, if the resource is invalid, or if
	/// we run out of memory.
	///
	/// # Safety
	///
	/// NVENC reads `pitch` bytes per row for the session's height (times the
	/// format's plane factor) from the buffer, and the buffer's length is not
	/// checked against that: it must be at least that large, and `pitch` must
	/// describe its rows. The buffer must also not be shared with CUDA work that
	/// writes it while a [`MappedResource`] of it is alive.
	pub unsafe fn register_cuda_resource(
		&self,
		pitch: u32,
		mapped_buffer: MappedBuffer,
	) -> Result<RegisteredResource<MappedBuffer>, EncodeError> {
		let stream = self.encoder.ctx.default_stream();
		// The read-recording guard drops here, before NVENC reads: a `MappedBuffer`
		// is never written, and its owner is released only after the unregister.
		let (device_ptr, _) = mapped_buffer.device_ptr(&stream);
		// SAFETY: `mapped_buffer` owns the allocation addressed by `device_ptr`
		// and is retained by the returned registration; the caller vouches for
		// `pitch` against its size.
		unsafe {
			self.register_generic_resource(
				mapped_buffer,
				NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
				device_ptr as *mut c_void,
				pitch,
			)
		}
	}

	/// Create a [`RegisteredResource`].
	///
	/// This function is generic in the marker. This is so that you can
	/// optionally put a value on the [`RegisteredResource`] to make sure that
	/// value does not get dropped while the resource is registered. You should
	/// prefer using specific functions for the resource you are registering,
	/// such as [`Session::register_cuda_resource`], when they are available.
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#input-buffers-allocated-externally).
	///
	/// # Errors
	///
	/// Could error if registration fails, if the resource is invalid, or if
	/// we run out of memory. A failed registration releases `marker`.
	///
	/// # Safety
	///
	/// `resource_to_register` must identify a live allocation of the requested
	/// type and dimensions. `marker` must own everything needed to keep that
	/// allocation valid until the returned resource is dropped.
	pub unsafe fn register_generic_resource<T>(
		&self,
		marker: T,
		resource_type: NV_ENC_INPUT_RESOURCE_TYPE,
		resource_to_register: *mut c_void,
		pitch: u32,
	) -> Result<RegisteredResource<T>, EncodeError> {
		let mut params = NV_ENC_REGISTER_RESOURCE::new(
			resource_type,
			self.width,
			self.height,
			resource_to_register,
			self.buffer_format,
		)
		.pitch(pitch);
		let registration = Registration::new(self.encoder.clone(), marker, &mut params)?;
		Ok(RegisteredResource { registration, pitch })
	}
}

/// Abstraction around input buffer allocated using
/// the NVIDIA Video Encoder API.
///
/// The buffer is automatically destroyed when dropped. It shares its session's
/// driver handle, which is not thread-safe, so it cannot leave its thread:
///
/// ```compile_fail,E0277
/// fn is_send<T: Send>() {}
/// is_send::<moq_nvenc::Buffer>();
/// ```
#[derive(Debug)]
pub struct Buffer {
	pub(crate) ptr: *mut c_void,
	pitch: u32,
	encoder: Arc<Encoder>,
}

impl Buffer {
	/// Lock the input buffer.
	///
	/// On a successful lock you get a [`BufferLock`] which can be used to write
	/// data to the input buffer. On drop, [`BufferLock`] will unlock the
	/// buffer.
	///
	/// This function will block until a lock is acquired. For the non-blocking
	/// version see [`Buffer::try_lock`].
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#input-buffers-allocated-through-nvidia-video-encoder-interface).
	///
	/// # Errors
	///
	/// Could error if we run out of memory.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::{
	/// #     sys::nvEncodeAPI::{
	/// #         NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB,
	/// #         NV_ENC_CODEC_H264_GUID,
	/// #         NV_ENC_PIC_PARAMS,
	/// #         NV_ENC_PIC_STRUCT,
	/// #     },
	/// #     Encoder, EncoderInitParams
	/// # };
	/// # const WIDTH: u32 = 1920;
	/// # const HEIGHT: u32 = 1080;
	/// # const DATA_LEN: usize = (WIDTH * HEIGHT * 4) as usize;
	/// //* Create encoder. *//
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// # let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	/// //* Set `encode_guid` and `buffer_format`, and check that H.264 encoding and the ARGB format are supported. *//
	/// # let encode_guid = NV_ENC_CODEC_H264_GUID;
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&encode_guid));
	/// # let buffer_format = NV_ENC_BUFFER_FORMAT_ARGB;
	/// # let input_formats = encoder.get_supported_input_formats(encode_guid).unwrap();
	/// # assert!(input_formats.contains(&buffer_format));
	/// //* Begin encoder session. *//
	/// # let mut initialize_params = EncoderInitParams::new(encode_guid, WIDTH, HEIGHT);
	/// # initialize_params.display_aspect_ratio(16, 9)
	/// #     .framerate(30, 1)
	/// #     .enable_picture_type_decision();
	/// # let session = encoder.start_session(
	/// #     buffer_format,
	/// #     initialize_params,
	/// # ).unwrap();
	///
	/// // Create an input buffer.
	/// let mut input_buffer = session
	///     .create_input_buffer()
	///     .unwrap();
	/// unsafe { input_buffer.lock().unwrap().write(&[0; DATA_LEN]) };
	/// ```
	pub fn lock(&mut self) -> Result<BufferLock<'_>, EncodeError> {
		self.lock_inner(true)
	}

	/// Non-blocking version of [`Buffer::lock`]. See it for more info.
	///
	/// This function will return an error with
	/// [`ErrorKind::EncoderBusy`](super::ErrorKind::EncoderBusy) or
	/// [`ErrorKind::LockBusy`](super::ErrorKind::LockBusy) if the lock is being
	/// used. The NVIDIA documentation from the header file is unclear about
	/// this.
	///
	/// # Errors
	///
	/// Could error if we run out of memory.
	///
	/// If this returns an error with
	/// [`ErrorKind::EncoderBusy`](super::ErrorKind::EncoderBusy) or
	/// [`ErrorKind::LockBusy`](super::ErrorKind::LockBusy) then that means the
	/// lock is still busy and the client should retry in a few
	/// milliseconds.
	pub fn try_lock(&mut self) -> Result<BufferLock<'_>, EncodeError> {
		self.lock_inner(false)
	}

	#[inline]
	fn lock_inner(&mut self, wait: bool) -> Result<BufferLock<'_>, EncodeError> {
		let mut lock_input_buffer_params = NV_ENC_LOCK_INPUT_BUFFER {
			version: NV_ENC_LOCK_INPUT_BUFFER_VER,
			inputBuffer: self.ptr,
			..Default::default()
		};
		if !wait {
			lock_input_buffer_params.set_doNotWait(1);
		}
		unsafe { (self.encoder.api.lock_input_buffer)(self.encoder.ptr, &mut lock_input_buffer_params) }
			.result(&self.encoder)?;

		let data_ptr = lock_input_buffer_params.bufferDataPtr;
		let pitch = lock_input_buffer_params.pitch;
		self.pitch = pitch;

		Ok(BufferLock {
			buffer: self,
			data_ptr,
			pitch,
		})
	}
}

impl Drop for Buffer {
	fn drop(&mut self) {
		let _ = unsafe { (self.encoder.api.destroy_input_buffer)(self.encoder.ptr, self.ptr) }.result(&self.encoder);
	}
}

impl sealed::Input for Buffer {
	fn abandon(self) {
		// Skip `Drop`: destroying the buffer could free memory the driver still reads.
		let this = ManuallyDrop::new(self);
		// SAFETY: read once from a value whose destructor never runs.
		drop(unsafe { ptr::read(&this.encoder) });
	}
}

impl EncoderInput for Buffer {
	fn pitch(&self) -> u32 {
		self.pitch
	}

	fn handle(&mut self) -> *mut c_void {
		self.ptr
	}

	fn encoder(&self) -> &Arc<Encoder> {
		&self.encoder
	}
}

/// An RAII lock on the input buffer.
///
/// This type is created via [`Buffer::lock`] or [`Buffer::try_lock`].
/// The purpose of this type is similar to [`std::sync::MutexGuard`] -
/// it automatically unlocks the buffer when the lock goes out of scope.
#[derive(Debug)]
pub struct BufferLock<'a> {
	buffer: &'a Buffer,
	data_ptr: *mut c_void,
	pitch: u32,
}

impl BufferLock<'_> {
	/// Write data to the buffer.
	///
	/// # Safety
	///
	/// The size of the data should be less or equal to the size of the buffer.
	/// The size of the buffer depends on the width, height, and buffer format.
	///
	/// The user should also account for pitch, the data is written
	/// contiguously.
	pub unsafe fn write(&mut self, data: &[u8]) {
		// TODO: Make this safe by doing checks.
		// - Check that length of data fits (depends on format).
		// - Write pitched?
		data.as_ptr().copy_to(self.data_ptr.cast::<u8>(), data.len());
	}

	/// The row stride (pitch), in bytes, NVENC chose for this input buffer. It
	/// may exceed the visible width, so a tightly-packed frame must be written
	/// row by row at this stride (a flat [`write`](Self::write) corrupts the
	/// image whenever pitch != width). See [`write_rows`](Self::write_rows).
	#[must_use]
	pub fn pitch(&self) -> u32 {
		self.pitch
	}

	/// Copy `rows` rows of `row_bytes` bytes each from tightly-packed `src` into
	/// the buffer, starting at byte offset `dst_offset` and placing successive
	/// rows `dst_stride` bytes apart. This is the pitched write needed for planar
	/// input (e.g. IYUV) whose plane row stride differs from the visible width.
	///
	/// # Safety
	///
	/// `dst_offset + (rows - 1) * dst_stride + row_bytes` must be within the
	/// buffer, `src` must hold at least `rows * row_bytes` bytes, and
	/// `row_bytes <= dst_stride`.
	pub unsafe fn write_rows(
		&mut self,
		dst_offset: usize,
		dst_stride: usize,
		src: &[u8],
		row_bytes: usize,
		rows: usize,
	) {
		let base = self.data_ptr.cast::<u8>();
		for row in 0..rows {
			let src_row = &src[row * row_bytes..row * row_bytes + row_bytes];
			unsafe {
				src_row
					.as_ptr()
					.copy_to_nonoverlapping(base.add(dst_offset + row * dst_stride), row_bytes);
			}
		}
	}
}

impl Drop for BufferLock<'_> {
	fn drop(&mut self) {
		let _ = unsafe { (self.buffer.encoder.api.unlock_input_buffer)(self.buffer.encoder.ptr, self.buffer.ptr) }
			.result(&self.buffer.encoder);
	}
}

/// Abstraction around the output bitstream buffer that
/// is used as the output of the encoding.
///
/// The buffer is automatically destroyed when dropped. It shares its session's
/// driver handle, which is not thread-safe, so it cannot leave its thread:
///
/// ```compile_fail,E0277
/// fn is_send<T: Send>() {}
/// is_send::<moq_nvenc::Bitstream>();
/// ```
#[derive(Debug)]
pub struct Bitstream {
	pub(crate) ptr: *mut c_void,
	pub(crate) encoder: Arc<Encoder>,
}

impl Bitstream {
	/// Lock the output bitstream.
	///
	/// On a successful lock you get a [`BitstreamLock`] which can be used to
	/// access the bitstream data as well as any other information the
	/// encoder provides when locking a bitstream.
	///
	/// This function will block until a lock is acquired. For the non-blocking
	/// version see [`Bitstream::try_lock`].
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#retrieving-encoded-output).
	///
	/// # Errors
	///
	/// Could error if we run out of memory.
	pub fn lock(&mut self) -> Result<BitstreamLock<'_>, EncodeError> {
		self.lock_inner(true)
	}

	/// Non-blocking version of [`Bitstream::lock`]. See it for more info.
	///
	/// This function will return an error with
	/// [`ErrorKind::LockBusy`](super::ErrorKind::LockBusy) if the
	/// lock is currently busy.
	///
	/// # Errors
	///
	/// Could error if we run out of memory.
	///
	/// An error with [`ErrorKind::LockBusy`](super::ErrorKind::LockBusy) could
	/// be returned if the lock is currently busy. This is a recoverable
	/// error and the client should retry in a few milliseconds.
	pub fn try_lock(&mut self) -> Result<BitstreamLock<'_>, EncodeError> {
		self.lock_inner(false)
	}

	fn lock_inner(&mut self, wait: bool) -> Result<BitstreamLock<'_>, EncodeError> {
		// Lock bitstream.
		let mut lock_bitstream_buffer_params = NV_ENC_LOCK_BITSTREAM {
			version: NV_ENC_LOCK_BITSTREAM_VER,
			outputBitstream: self.ptr,
			..Default::default()
		};
		if !wait {
			lock_bitstream_buffer_params.set_doNotWait(1);
		}
		unsafe { (self.encoder.api.lock_bitstream)(self.encoder.ptr, &mut lock_bitstream_buffer_params) }
			.result(&self.encoder)?;

		let data_ptr = lock_bitstream_buffer_params.bitstreamBufferPtr.cast::<u8>();
		let data_size = lock_bitstream_buffer_params.bitstreamSizeInBytes as usize;
		// SAFETY: the driver keeps the output valid until the unlock `BitstreamLock` performs on drop.
		let Some(data) = (unsafe { locked_data(data_ptr, data_size) }) else {
			// The caller never receives a lock to drop, so unlock here.
			let _ = unsafe { (self.encoder.api.unlock_bitstream)(self.encoder.ptr, self.ptr) }.result(&self.encoder);
			return Err(EncodeError::new(
				ErrorKind::InvalidPtr,
				Some("the driver locked a non-empty bitstream at a null address".into()),
			));
		};

		Ok(BitstreamLock {
			bitstream: self,
			data,
			frame_index: lock_bitstream_buffer_params.frameIdx,
			timestamp: lock_bitstream_buffer_params.outputTimeStamp,
			duration: lock_bitstream_buffer_params.outputDuration,
			picture_type: lock_bitstream_buffer_params.pictureType,
		})
	}

	/// Give up a bitstream the driver may still write: leak its handle, but
	/// release its encoder reference so the session can still be destroyed.
	pub(crate) fn abandon(self) {
		let this = ManuallyDrop::new(self);
		// SAFETY: read once from a value whose destructor never runs.
		drop(unsafe { ptr::read(&this.encoder) });
	}
}

impl Drop for Bitstream {
	fn drop(&mut self) {
		let _ =
			unsafe { (self.encoder.api.destroy_bitstream_buffer)(self.encoder.ptr, self.ptr) }.result(&self.encoder);
	}
}

/// The bytes the driver locked, empty for an empty output. `None` for a null
/// address with a nonzero size, which no slice can describe.
///
/// # Safety
///
/// A non-null `ptr` must be valid for reads of `len` bytes for `'a`.
unsafe fn locked_data<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
	if len == 0 {
		// An empty output may come with a null address, which a slice must not hold.
		return Some(&[]);
	}
	if ptr.is_null() {
		return None;
	}
	Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// An RAII lock on the output bitstream buffer.
///
/// This type is created via [`Bitstream::lock`] or [`Bitstream::try_lock`].
/// The purpose of this type is similar to [`std::sync::MutexGuard`] -
/// it automatically unlocks the buffer when the lock goes out of scope.
#[derive(Debug)]
pub struct BitstreamLock<'a> {
	bitstream: &'a Bitstream,
	data: &'a [u8],
	// statistics and other info
	frame_index: u32,
	timestamp: u64,
	duration: u64,
	picture_type: NV_ENC_PIC_TYPE,
	// TODO: other fields
}

impl BitstreamLock<'_> {
	/// Getter for the data contained in the output bitstream.
	#[must_use]
	pub fn data(&self) -> &[u8] {
		self.data
	}

	/// Getter for the frame index.
	#[must_use]
	pub fn frame_index(&self) -> u32 {
		self.frame_index
	}

	/// Getter for the timestamp.
	#[must_use]
	pub fn timestamp(&self) -> u64 {
		self.timestamp
	}

	/// Getter for the duration.
	#[must_use]
	pub fn duration(&self) -> u64 {
		self.duration
	}

	/// Getter for the picture type.
	#[must_use]
	pub fn picture_type(&self) -> NV_ENC_PIC_TYPE {
		self.picture_type
	}
}

impl Drop for BitstreamLock<'_> {
	fn drop(&mut self) {
		let _ =
			unsafe { (self.bitstream.encoder.api.unlock_bitstream)(self.bitstream.encoder.ptr, self.bitstream.ptr) }
				.result(&self.bitstream.encoder);
	}
}

/// An external resource registered with one session.
///
/// The Encoder API exposes a way to use input buffers allocated externally,
/// for example through CUDA or OpenGL. Register the allocation once, then
/// [`map`](Self::map) it for each encode: registration is the expensive half,
/// and NVENC forbids touching the allocation only while it is mapped.
///
/// The resource is unregistered when dropped, and only then is its owner
/// released. The external buffer memory should still be properly destroyed
/// by the client.
///
/// Like the session it belongs to, it cannot leave its thread:
///
/// ```compile_fail,E0277
/// fn is_send<T: Send>() {}
/// is_send::<moq_nvenc::RegisteredResource<()>>();
/// ```
#[derive(Debug)]
pub struct RegisteredResource<T> {
	registration: Registration<Arc<Encoder>, T>,
	pitch: u32,
}

impl<T> RegisteredResource<T> {
	/// Map the resource for one [`Session::encode_picture`]. It is unmapped when
	/// the returned input drops, which [`Submission::finish`](super::Submission::finish)
	/// hands back once NVENC has finished reading it; write the allocation only
	/// while it is unmapped.
	///
	/// Mapping waits for CUDA work on the default stream to finish; work on any
	/// other stream must be synchronized first.
	///
	/// # Errors
	///
	/// Could error if the driver cannot map the resource; the registration
	/// stays usable. Errors with [`ErrorKind::InvalidCall`](super::ErrorKind::InvalidCall)
	/// if an earlier mapping was never unmapped, because it was forgotten,
	/// abandoned, or failed to unmap: NVENC may still hold it, so the
	/// registration is retired and its allocation must not be written again.
	pub fn map(&mut self) -> Result<MappedResource<'_, T>, EncodeError> {
		let pitch = self.pitch;
		Ok(MappedResource {
			mapped: self.registration.map()?,
			pitch,
		})
	}
}

/// A [`RegisteredResource`] mapped for one encode, unmapped on drop.
#[derive(Debug)]
pub struct MappedResource<'a, T> {
	mapped: Mapped<'a, Arc<Encoder>, T>,
	pitch: u32,
}

impl<T> MappedResource<'_, T> {
	/// Unmap now, reporting a failure that dropping would swallow.
	///
	/// # Errors
	///
	/// Could error if the driver cannot unmap the resource. NVENC may then
	/// still hold it, so the registration and its allocation are leaked and
	/// the allocation must not be written again.
	pub fn unmap(self) -> Result<(), EncodeError> {
		self.mapped.unmap()
	}
}

/// A registered external resource plus the owner keeping its allocation alive.
#[derive(Debug)]
struct Registration<A: ResourceApi, T> {
	reg_ptr: *mut c_void,
	api: A,
	// Dropped only once the resource is unregistered.
	marker: ManuallyDrop<T>,
	/// Set before a mapping is handed out and cleared only once it unmaps, so a
	/// mapping that never does (forgotten, abandoned, or failed) leaves the
	/// registration and the owner of its memory leaked rather than released.
	mapped: bool,
}

impl<A: ResourceApi, T> Registration<A, T> {
	/// Register `marker`'s allocation. A failed registration holds nothing, so
	/// `marker` is released.
	fn new(api: A, marker: T, params: &mut NV_ENC_REGISTER_RESOURCE) -> Result<Self, EncodeError> {
		let reg_ptr = api.register_resource(params)?;
		Ok(Self {
			reg_ptr,
			api,
			marker: ManuallyDrop::new(marker),
			mapped: false,
		})
	}

	fn map(&mut self) -> Result<Mapped<'_, A, T>, EncodeError> {
		if self.mapped {
			return Err(EncodeError::new(
				ErrorKind::InvalidCall,
				Some("an earlier mapping was never unmapped".into()),
			));
		}
		let map_ptr = self.api.map_input_resource(self.reg_ptr)?;
		self.mapped = true;
		Ok(Mapped {
			registration: self,
			map_ptr,
		})
	}
}

/// Unregister the external resource when it goes out of scope.
impl<A: ResourceApi, T> Drop for Registration<A, T> {
	fn drop(&mut self) {
		// NVENC may still read a mapping that never unmapped.
		if self.mapped {
			return;
		}
		// NVENC may still refer to an allocation it failed to unregister, so
		// leak its owner rather than free it.
		if self.api.unregister_resource(self.reg_ptr).is_ok() {
			// SAFETY: dropped once, here, and never touched again.
			unsafe { ManuallyDrop::drop(&mut self.marker) };
		}
	}
}

/// One mapping of a [`Registration`].
#[derive(Debug)]
struct Mapped<'a, A: ResourceApi, T> {
	registration: &'a mut Registration<A, T>,
	map_ptr: *mut c_void,
}

impl<A: ResourceApi, T> Mapped<'_, A, T> {
	/// Give up a mapping the driver may still be reading: never unmap it, so the
	/// registration stays marked mapped and is never unregistered or released.
	fn abandon(self) {
		std::mem::forget(self);
	}

	fn unmap(self) -> Result<(), EncodeError> {
		let mut this = ManuallyDrop::new(self);
		this.release()
	}

	/// A failed unmap leaves the registration marked mapped: unregistering or
	/// freeing it could release memory NVENC still holds.
	fn release(&mut self) -> Result<(), EncodeError> {
		self.registration.api.unmap_input_resource(self.map_ptr)?;
		self.registration.mapped = false;
		Ok(())
	}
}

impl<A: ResourceApi, T> Drop for Mapped<'_, A, T> {
	fn drop(&mut self) {
		let _ = self.release();
	}
}

impl<T> sealed::Input for MappedResource<'_, T> {
	fn abandon(self) {
		self.mapped.abandon();
	}
}

impl<T> EncoderInput for MappedResource<'_, T> {
	fn pitch(&self) -> u32 {
		self.pitch
	}

	fn handle(&mut self) -> *mut c_void {
		self.mapped.map_ptr
	}

	fn encoder(&self) -> &Arc<Encoder> {
		&self.mapped.registration.api
	}
}

#[cfg(test)]
mod tests {
	use std::{
		cell::{Cell, RefCell},
		rc::Rc,
	};

	use super::*;

	#[derive(Debug, Default, PartialEq, Eq)]
	struct Calls {
		register: usize,
		map: usize,
		unmap: usize,
		unregister: usize,
	}

	#[derive(Debug)]
	struct TestApi {
		calls: RefCell<Calls>,
		mapped: Cell<bool>,
		owner_alive: Rc<Cell<bool>>,
		map_error: Option<ErrorKind>,
		unmap_error: Option<ErrorKind>,
		unregister_error: Option<ErrorKind>,
	}

	impl TestApi {
		fn new(owner_alive: Rc<Cell<bool>>) -> Self {
			Self {
				calls: RefCell::new(Calls::default()),
				mapped: Cell::new(false),
				owner_alive,
				map_error: None,
				unmap_error: None,
				unregister_error: None,
			}
		}

		fn handle() -> *mut c_void {
			std::ptr::NonNull::<u8>::dangling().as_ptr().cast()
		}

		fn assert_owner_alive(&self) {
			assert!(
				self.owner_alive.get(),
				"input owner was dropped before cleanup finished"
			);
		}
	}

	impl ResourceApi for &TestApi {
		fn register_resource(&self, _params: &mut NV_ENC_REGISTER_RESOURCE) -> Result<*mut c_void, EncodeError> {
			self.assert_owner_alive();
			self.calls.borrow_mut().register += 1;
			Ok(TestApi::handle())
		}

		fn map_input_resource(&self, _registered: *mut c_void) -> Result<*mut c_void, EncodeError> {
			self.assert_owner_alive();
			self.calls.borrow_mut().map += 1;
			match self.map_error {
				Some(kind) => Err(EncodeError::new(kind, None)),
				None => {
					self.mapped.set(true);
					Ok(TestApi::handle())
				}
			}
		}

		fn unmap_input_resource(&self, _mapped: *mut c_void) -> Result<(), EncodeError> {
			self.assert_owner_alive();
			self.calls.borrow_mut().unmap += 1;
			match self.unmap_error {
				Some(kind) => Err(EncodeError::new(kind, None)),
				None => {
					self.mapped.set(false);
					Ok(())
				}
			}
		}

		fn unregister_resource(&self, _registered: *mut c_void) -> Result<(), EncodeError> {
			self.assert_owner_alive();
			assert!(!self.mapped.get(), "unregistered a resource that is still mapped");
			self.calls.borrow_mut().unregister += 1;
			match self.unregister_error {
				Some(kind) => Err(EncodeError::new(kind, None)),
				None => Ok(()),
			}
		}
	}

	#[derive(Debug)]
	struct Owner(Rc<Cell<bool>>);

	impl Drop for Owner {
		fn drop(&mut self) {
			assert!(self.0.replace(false), "input owner dropped more than once");
		}
	}

	fn setup() -> (Owner, Rc<Cell<bool>>) {
		let alive = Rc::new(Cell::new(true));
		(Owner(alive.clone()), alive)
	}

	fn register(api: &TestApi, owner: Owner) -> Result<Registration<&TestApi, Owner>, EncodeError> {
		let mut params = NV_ENC_REGISTER_RESOURCE::new(
			NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
			1920,
			1080,
			TestApi::handle(),
			NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12,
		)
		.pitch(1920);
		Registration::new(api, owner, &mut params)
	}

	#[test]
	fn locked_data_never_builds_a_slice_from_null() {
		// An empty output may carry any address; a null one still reads as empty.
		assert_eq!(unsafe { locked_data(ptr::null(), 0) }, Some(&[][..]));
		assert_eq!(unsafe { locked_data(ptr::null(), 4) }, None);
		let bytes = [1u8, 2, 3];
		assert_eq!(unsafe { locked_data(bytes.as_ptr(), 3) }, Some(&bytes[..]));
		assert_eq!(unsafe { locked_data(bytes.as_ptr(), 0) }, Some(&[][..]));
	}

	#[test]
	fn one_registration_serves_every_mapping() {
		let (owner, alive) = setup();
		let api = TestApi::new(alive.clone());

		let mut registration = register(&api, owner).expect("registration should succeed");
		for _ in 0..3 {
			drop(registration.map().expect("mapping should succeed"));
			assert!(!api.mapped.get(), "each mapping is released before the next");
		}
		assert!(alive.get());
		drop(registration);

		assert!(!alive.get(), "owner should be released after destruction");
		assert_eq!(
			*api.calls.borrow(),
			Calls {
				register: 1,
				map: 3,
				unmap: 3,
				unregister: 1,
			}
		);
	}

	#[test]
	fn mapping_failure_keeps_the_registration() {
		let (owner, alive) = setup();
		let mut api = TestApi::new(alive.clone());
		api.map_error = Some(ErrorKind::MapFailed);

		let mut registration = register(&api, owner).expect("registration should succeed");
		let error = registration.map().expect_err("mapping should fail");
		assert_eq!(error.kind(), ErrorKind::MapFailed);
		assert!(alive.get());
		drop(registration);

		assert!(!alive.get(), "owner should be released after unregistering");
		assert_eq!(
			*api.calls.borrow(),
			Calls {
				register: 1,
				map: 1,
				unmap: 0,
				unregister: 1,
			}
		);
	}

	#[test]
	fn failed_unmap_retains_the_owner() {
		let (owner, alive) = setup();
		let mut api = TestApi::new(alive.clone());
		api.unmap_error = Some(ErrorKind::ResourceNotMapped);

		let mut registration = register(&api, owner).expect("registration should succeed");
		let error = registration
			.map()
			.expect("mapping should succeed")
			.unmap()
			.expect_err("unmapping should fail");
		assert_eq!(error.kind(), ErrorKind::ResourceNotMapped);
		drop(registration);

		assert!(alive.get(), "a possibly mapped allocation must remain owned");
		assert_eq!(
			*api.calls.borrow(),
			Calls {
				register: 1,
				map: 1,
				unmap: 1,
				unregister: 0,
			}
		);
	}

	#[test]
	fn an_abandoned_mapping_retains_the_owner() {
		let (owner, alive) = setup();
		let api = TestApi::new(alive.clone());

		let mut registration = register(&api, owner).expect("registration should succeed");
		registration.map().expect("mapping should succeed").abandon();
		drop(registration);

		assert!(alive.get(), "an allocation the driver may still read must remain owned");
		assert_eq!(
			*api.calls.borrow(),
			Calls {
				register: 1,
				map: 1,
				unmap: 0,
				unregister: 0,
			}
		);
	}

	#[test]
	fn a_forgotten_mapping_retains_the_owner() {
		let (owner, alive) = setup();
		let api = TestApi::new(alive.clone());

		let mut registration = register(&api, owner).expect("registration should succeed");
		// Safe code can skip every destructor, so this must not rely on one.
		std::mem::forget(registration.map().expect("mapping should succeed"));
		drop(registration);

		assert!(alive.get(), "an allocation the driver may still read must remain owned");
		assert_eq!(
			*api.calls.borrow(),
			Calls {
				register: 1,
				map: 1,
				unmap: 0,
				unregister: 0,
			}
		);
	}

	#[test]
	fn a_mapping_never_unmapped_refuses_the_next() {
		let (owner, alive) = setup();
		let mut api = TestApi::new(alive.clone());
		api.unmap_error = Some(ErrorKind::ResourceNotMapped);

		let mut registration = register(&api, owner).expect("registration should succeed");
		std::mem::forget(registration.map().expect("mapping should succeed"));
		let error = registration
			.map()
			.expect_err("a forgotten mapping should refuse the next");
		assert_eq!(error.kind(), ErrorKind::InvalidCall);

		let mut registration = register(&api, Owner(alive.clone())).expect("registration should succeed");
		registration.map().expect("mapping should succeed").abandon();
		let error = registration
			.map()
			.expect_err("an abandoned mapping should refuse the next");
		assert_eq!(error.kind(), ErrorKind::InvalidCall);

		let mut registration = register(&api, Owner(alive.clone())).expect("registration should succeed");
		let _ = registration.map().expect("mapping should succeed").unmap();
		let error = registration.map().expect_err("a failed unmap should refuse the next");
		assert_eq!(error.kind(), ErrorKind::InvalidCall);

		assert_eq!(api.calls.borrow().map, 3, "a refused mapping never reaches the driver");
	}

	#[test]
	fn failed_unregister_on_drop_retains_the_owner() {
		let (owner, alive) = setup();
		let mut api = TestApi::new(alive.clone());
		api.unregister_error = Some(ErrorKind::ResourceNotRegistered);

		drop(register(&api, owner).expect("registration should succeed"));

		assert!(alive.get(), "a possibly registered allocation must remain owned");
		assert_eq!(
			*api.calls.borrow(),
			Calls {
				register: 1,
				map: 0,
				unmap: 0,
				unregister: 1,
			}
		);
	}
}

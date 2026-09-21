//! Defines traits and types for dealing with input and output buffers.

use std::{ffi::c_void, ptr, sync::Arc};

use cudarc::driver::{DevicePtr, MappedBuffer};

use super::{encoder::Encoder, result::EncodeError, session::Session};
use crate::sys::nvEncodeAPI::{
	NV_ENC_BUFFER_FORMAT, NV_ENC_CREATE_BITSTREAM_BUFFER, NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
	NV_ENC_CREATE_INPUT_BUFFER, NV_ENC_CREATE_INPUT_BUFFER_VER, NV_ENC_INPUT_RESOURCE_TYPE, NV_ENC_LOCK_BITSTREAM,
	NV_ENC_LOCK_BITSTREAM_VER, NV_ENC_LOCK_INPUT_BUFFER, NV_ENC_LOCK_INPUT_BUFFER_VER, NV_ENC_MAP_INPUT_RESOURCE,
	NV_ENC_MAP_INPUT_RESOURCE_VER, NV_ENC_PIC_TYPE, NV_ENC_REGISTER_RESOURCE,
};

mod sealed {
	pub trait Input {}
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
	/// Could error if registration or mapping fails,
	/// if the resource is invalid, or if we run out of memory.
	pub fn register_cuda_resource(
		&self,
		pitch: u32,
		mapped_buffer: MappedBuffer,
	) -> Result<RegisteredResource<MappedBuffer>, EncodeError> {
		let stream = self.encoder.ctx.default_stream();
		let (device_ptr, _) = mapped_buffer.device_ptr(&stream);
		// SAFETY: `mapped_buffer` owns the allocation addressed by `device_ptr`
		// and is retained by the returned registration.
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
	/// Could error if registration or mapping fails,
	/// if the resource is invalid, or if we run out of memory.
	/// A mapping failure rolls registration back before releasing `marker`. If
	/// rollback also fails, [`EncodeError::cleanup`] exposes that failure and
	/// the marker is retained because NVENC may still refer to its allocation.
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
		let mapping = Mapping::new(self.encoder.clone(), marker, &mut params)?;
		Ok(RegisteredResource { mapping, pitch })
	}
}

/// Abstraction around input buffer allocated using
/// the NVIDIA Video Encoder API.
///
/// The buffer is automatically destroyed when dropped.
#[derive(Debug)]
pub struct Buffer {
	pub(crate) ptr: *mut c_void,
	pitch: u32,
	encoder: Arc<Encoder>,
}

unsafe impl Send for Buffer {}

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

impl sealed::Input for Buffer {}

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
#[allow(clippy::module_name_repetitions)]
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
/// The buffer is automatically destroyed when dropped.
#[derive(Debug)]
pub struct Bitstream {
	pub(crate) ptr: *mut c_void,
	pub(crate) encoder: Arc<Encoder>,
}

unsafe impl Send for Bitstream {}

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

		// Get data.
		let data_ptr = lock_bitstream_buffer_params.bitstreamBufferPtr;
		let data_size = lock_bitstream_buffer_params.bitstreamSizeInBytes as usize;
		let data = unsafe { std::slice::from_raw_parts_mut(data_ptr.cast::<u8>(), data_size) };

		Ok(BitstreamLock {
			bitstream: self,
			data,
			frame_index: lock_bitstream_buffer_params.frameIdx,
			timestamp: lock_bitstream_buffer_params.outputTimeStamp,
			duration: lock_bitstream_buffer_params.outputDuration,
			picture_type: lock_bitstream_buffer_params.pictureType,
		})
	}
}

impl Drop for Bitstream {
	fn drop(&mut self) {
		let _ =
			unsafe { (self.encoder.api.destroy_bitstream_buffer)(self.encoder.ptr, self.ptr) }.result(&self.encoder);
	}
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

/// Abstraction for a registered and mapped external resource.
///
/// The Encoder API exposes a way to use input buffers allocated externally,
/// for example through CUDA or OpenGL.
///
/// The buffer is automatically unmapped and unregistered when dropped.
/// The external buffer memory should still be properly destroyed by the client.
#[derive(Debug)]
pub struct RegisteredResource<T> {
	mapping: Mapping<Arc<Encoder>, T>,
	pitch: u32,
}

unsafe impl Send for RegisteredResource<MappedBuffer> {}

/// A registered and mapped external resource plus the owner keeping its
/// allocation alive.
#[derive(Debug)]
struct Mapping<A: ResourceApi, T> {
	reg_ptr: *mut c_void,
	map_ptr: *mut c_void,
	api: A,
	// Dropped after the resource is unregistered.
	_marker: T,
}

impl<A: ResourceApi, T> Mapping<A, T> {
	/// Register and map as one transaction: a mapping failure unregisters
	/// before `marker` is released.
	fn new(api: A, marker: T, params: &mut NV_ENC_REGISTER_RESOURCE) -> Result<Self, EncodeError> {
		let reg_ptr = api.register_resource(params)?;
		let map_ptr = match api.map_input_resource(reg_ptr) {
			Ok(map_ptr) => map_ptr,
			Err(primary) => {
				if let Err(cleanup) = api.unregister_resource(reg_ptr) {
					// NVENC may still refer to the allocation and there is no handle
					// left to retry with, so leaking it is safer than freeing it.
					std::mem::forget(marker);
					return Err(primary.with_cleanup(cleanup));
				}
				return Err(primary);
			}
		};
		Ok(Self {
			reg_ptr,
			map_ptr,
			api,
			_marker: marker,
		})
	}
}

/// Automatically unmap and unregister the external resource
/// when it goes out of scope.
impl<A: ResourceApi, T> Drop for Mapping<A, T> {
	fn drop(&mut self) {
		let _ = self.api.unmap_input_resource(self.map_ptr);
		let _ = self.api.unregister_resource(self.reg_ptr);
	}
}

impl<T> sealed::Input for RegisteredResource<T> {}

impl<T> EncoderInput for RegisteredResource<T> {
	fn pitch(&self) -> u32 {
		self.pitch
	}

	fn handle(&mut self) -> *mut c_void {
		self.mapping.map_ptr
	}

	fn encoder(&self) -> &Arc<Encoder> {
		&self.mapping.api
	}
}

#[cfg(test)]
mod tests {
	use std::{
		cell::{Cell, RefCell},
		error::Error,
		rc::Rc,
	};

	use super::*;
	use crate::safe::result::ErrorKind;

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
		owner_alive: Rc<Cell<bool>>,
		map_error: Option<ErrorKind>,
		unregister_error: Option<ErrorKind>,
	}

	impl TestApi {
		fn new(owner_alive: Rc<Cell<bool>>) -> Self {
			Self {
				calls: RefCell::new(Calls::default()),
				owner_alive,
				map_error: None,
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
				None => Ok(TestApi::handle()),
			}
		}

		fn unmap_input_resource(&self, _mapped: *mut c_void) -> Result<(), EncodeError> {
			self.assert_owner_alive();
			self.calls.borrow_mut().unmap += 1;
			Ok(())
		}

		fn unregister_resource(&self, _registered: *mut c_void) -> Result<(), EncodeError> {
			self.assert_owner_alive();
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

	fn register(api: &TestApi, owner: Owner) -> Result<Mapping<&TestApi, Owner>, EncodeError> {
		let mut params = NV_ENC_REGISTER_RESOURCE::new(
			NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
			1920,
			1080,
			TestApi::handle(),
			NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12,
		)
		.pitch(1920);
		Mapping::new(api, owner, &mut params)
	}

	#[test]
	fn mapping_failure_unregisters_before_releasing_the_owner() {
		let (owner, alive) = setup();
		let mut api = TestApi::new(alive.clone());
		api.map_error = Some(ErrorKind::MapFailed);

		let error = register(&api, owner).expect_err("mapping should fail");

		assert_eq!(error.kind(), ErrorKind::MapFailed);
		assert!(error.cleanup().is_none());
		assert!(!alive.get(), "owner should be released after successful rollback");
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
	fn rollback_failure_retains_both_errors_and_the_owner() {
		let (owner, alive) = setup();
		let mut api = TestApi::new(alive.clone());
		api.map_error = Some(ErrorKind::MapFailed);
		api.unregister_error = Some(ErrorKind::ResourceNotRegistered);

		let error = register(&api, owner).expect_err("mapping and rollback should fail");

		assert_eq!(error.kind(), ErrorKind::MapFailed);
		assert_eq!(
			error.cleanup().map(EncodeError::kind),
			Some(ErrorKind::ResourceNotRegistered)
		);
		assert_eq!(
			error
				.source()
				.and_then(|source| source.downcast_ref::<EncodeError>())
				.map(EncodeError::kind),
			Some(ErrorKind::ResourceNotRegistered)
		);
		assert!(alive.get(), "a possibly registered allocation must remain owned");
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
	fn mapped_resource_cleans_up_once_before_releasing_the_owner() {
		let (owner, alive) = setup();
		let api = TestApi::new(alive.clone());

		let resource = register(&api, owner).expect("mapping should succeed");
		assert!(alive.get());
		drop(resource);

		assert!(!alive.get(), "owner should be released after destruction");
		assert_eq!(
			*api.calls.borrow(),
			Calls {
				register: 1,
				map: 1,
				unmap: 1,
				unregister: 1,
			}
		);
	}
}

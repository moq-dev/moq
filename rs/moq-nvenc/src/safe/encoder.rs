//! The [`Encoder`] is the main entrypoint for the Encoder API.
//!
//! The [`Encoder`] provides a slightly higher-level abstraction over the
//! encoder API. This module also defines builders for some of the parameter
//! structs used by the interface.

use std::{ffi::c_void, ptr, sync::Arc};

use cudarc::driver::CudaContext;

use super::{
	api::{self, EncodeAPI, LoadError},
	result::EncodeError,
	session::Session,
};
use crate::sys::nvEncodeAPI::{
	GUID, NVENCAPI_VERSION, NV_ENC_BUFFER_FORMAT, NV_ENC_CONFIG, NV_ENC_CONFIG_VER, NV_ENC_DEVICE_TYPE,
	NV_ENC_INITIALIZE_PARAMS, NV_ENC_INITIALIZE_PARAMS_VER, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
	NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER, NV_ENC_PRESET_CONFIG, NV_ENC_PRESET_CONFIG_VER, NV_ENC_TUNING_INFO,
};

/// Entrypoint for the Encoder API.
///
/// The general usage follows these steps:
/// 1. Initialize the encoder.
/// 2. Set up the desired encoding parameters.
/// 3. Allocate or register input and output buffers.
/// 4. Copy frames to input buffers, encode, and read out of output bitstream.
/// 5. Close the encoding session and clean up.
///
/// With this wrapper cleanup is performed automatically.
/// To do the other steps this struct provides associated functions
/// such as [`Encoder::get_encode_guids`] or
/// [`Encoder::get_supported_input_formats`].
///
/// Once the configuration is completed, a session should be initialized with
/// [`Encoder::start_session`] to get a [`Session`].
/// This type has further function to create input and output buffers
/// and encode pictures.
///
/// See [NVIDIA Video Codec SDK - Video Encoder API Programming Guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html).
#[derive(Debug)]
pub struct Encoder {
	pub(crate) ptr: *mut c_void,
	pub(crate) api: &'static EncodeAPI,
	// Used to fetch the device pointer for an externally allocated buffer
	pub(crate) ctx: Arc<CudaContext>,
}

/// The client must flush the encoder before freeing any resources.
/// Do this by sending an EOS encode frame.
/// (This is also done automatically when [`Session`] is dropped.).
///
/// Sending an EOS frame might still generate data, so if you care
/// about this you should send an EOS frame yourself.
///
/// The client must free all the input and output resources before
/// destroying the encoder.
/// If using events, they must also be unregistered.
impl Drop for Encoder {
	fn drop(&mut self) {
		let _ = unsafe { (self.api.destroy_encoder)(self.ptr) }.result(self);
	}
}

impl Encoder {
	/// Load and validate the NVIDIA encode driver API.
	///
	/// # Errors
	///
	/// Returns an error when the driver library or an entry point is missing,
	/// the driver is too old, or API initialization fails.
	pub fn load() -> Result<(), LoadError> {
		api::get().map(|_| ())
	}

	/// Create an [`Encoder`] with CUDA as the encode device.
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#cuda).
	///
	/// # Errors
	///
	/// Returns an error if the NVENC library or a required entry point is
	/// unavailable, the NVIDIA driver is too old, or the encode device is
	/// unsupported or invalid.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::Encoder;
	/// let cuda_ctx = CudaContext::new(0).unwrap();
	/// let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	/// ```
	pub fn initialize_with_cuda(cuda_ctx: Arc<CudaContext>) -> Result<Self, LoadError> {
		let api = api::get()?;
		let mut session_params = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
			version: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
			deviceType: NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_CUDA,
			apiVersion: NVENCAPI_VERSION,
			// Pass the CUDA Context as the device.
			// valid casting since CUcontext is a *mut
			device: cuda_ctx.cu_ctx().cast::<c_void>(),
			..Default::default()
		};
		let encoder = open_session(
			|encoder| unsafe { (api.open_encode_session_ex)(&mut session_params, encoder) }.result_without_string(),
			|encoder| unsafe { (api.destroy_encoder)(encoder) }.result_without_string(),
		)?;

		Ok(Self {
			ptr: encoder,
			api,
			ctx: cuda_ctx,
		})
	}

	// TODO:
	// - Make Encoder generic in Device.
	// - Add functions to create Encoder from other encode devices.

	/// Get the encode GUIDs which the encoder supports.
	///
	/// You should use this function to check whether your
	/// machine supports the video compression standard
	/// that you with to use.
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#selecting-encoder-codec-guid).
	///
	/// # Errors
	///
	/// Could error if we run out of memory.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::{sys::nvEncodeAPI::NV_ENC_CODEC_H264_GUID, Encoder};
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	/// let encode_guids = encoder.get_encode_guids().unwrap();
	/// // Confirm that this machine support encoding to H.264.
	/// assert!(encode_guids.contains(&NV_ENC_CODEC_H264_GUID));
	/// ```
	pub fn get_encode_guids(&self) -> Result<Vec<GUID>, EncodeError> {
		// Query number of supported encoder codec GUIDs.
		let mut supported_count = 0;
		unsafe { (self.api.get_encode_guid_count)(self.ptr, &mut supported_count) }.result(self)?;
		// Get the supported GUIDs.
		let mut encode_guids = vec![GUID::default(); supported_count as usize];
		let mut actual_count = 0;
		unsafe { (self.api.get_encode_guids)(self.ptr, encode_guids.as_mut_ptr(), supported_count, &mut actual_count) }
			.result(self)?;
		encode_guids.truncate(actual_count as usize);
		Ok(encode_guids)
	}

	/// Get the encode preset GUIDs which the encoder supports
	/// for the given codec GUID.
	///
	/// You should use this function to check whether your
	/// machine supports the encode preset that you wish to use.
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#selecting-encoder-preset-configuration).
	///
	/// # Errors
	///
	/// Could error if the encode GUID is invalid
	/// or we run out of memory.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::{
	/// #     sys::nvEncodeAPI::{NV_ENC_CODEC_H264_GUID, NV_ENC_PRESET_P1_GUID},
	/// #     Encoder,
	/// # };
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	///
	/// //* Check if H.264 encoding is supported. *//
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&NV_ENC_CODEC_H264_GUID));
	///
	/// let preset_guids = encoder.get_preset_guids(NV_ENC_CODEC_H264_GUID).unwrap();
	/// // Confirm that H.264 supports the P1 preset (high performance, low quality) on this machine.
	/// assert!(preset_guids.contains(&NV_ENC_PRESET_P1_GUID));
	/// ```
	pub fn get_preset_guids(&self, encode_guid: GUID) -> Result<Vec<GUID>, EncodeError> {
		// Query the number of preset GUIDS.
		let mut preset_count = 0;
		unsafe { (self.api.get_encode_preset_count)(self.ptr, encode_guid, &mut preset_count) }.result(self)?;
		// Get the preset GUIDs.
		let mut actual_count = 0;
		let mut preset_guids = vec![GUID::default(); preset_count as usize];
		unsafe {
			(self.api.get_encode_preset_guids)(
				self.ptr,
				encode_guid,
				preset_guids.as_mut_ptr(),
				preset_count,
				&mut actual_count,
			)
		}
		.result(self)?;
		preset_guids.truncate(actual_count as usize);
		Ok(preset_guids)
	}

	/// Get the encode profile GUIDs which the encoder supports
	/// for the given codec GUID.
	///
	/// You should use this function to check whether your
	/// machine supports the encode profile that you wish to use.
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#selecting-an-encoder-profile).
	///
	/// # Errors
	///
	/// Could error if the encode GUID is invalid
	/// or we run out of memory.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::{
	/// #     sys::nvEncodeAPI::{NV_ENC_CODEC_H264_GUID, NV_ENC_H264_PROFILE_HIGH_GUID},
	/// #     Encoder,
	/// # };
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	///
	/// //* Check if H.264 encoding is supported. *//
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&NV_ENC_CODEC_H264_GUID));
	///
	/// let profile_guids = encoder.get_profile_guids(NV_ENC_CODEC_H264_GUID).unwrap();
	/// // Confirm that H.264 supports the HIGH profile on this machine.
	/// assert!(profile_guids.contains(&NV_ENC_H264_PROFILE_HIGH_GUID));
	/// ```
	pub fn get_profile_guids(&self, encode_guid: GUID) -> Result<Vec<GUID>, EncodeError> {
		// Query the number of profile GUIDs.
		let mut profile_count = 0;
		unsafe { (self.api.get_encode_profile_guid_count)(self.ptr, encode_guid, &mut profile_count) }.result(self)?;
		// Get the profile GUIDs.
		let mut profile_guids = vec![GUID::default(); profile_count as usize];
		let mut actual_count = 0;
		unsafe {
			(self.api.get_encode_profile_guids)(
				self.ptr,
				encode_guid,
				profile_guids.as_mut_ptr(),
				profile_count,
				&mut actual_count,
			)
		}
		.result(self)?;
		profile_guids.truncate(actual_count as usize);
		Ok(profile_guids)
	}

	/// Get the buffer formats which the encoder supports
	/// for the given codec GUID.
	///
	/// You should use this function to check whether your
	/// machine supports the buffer format that you wish to use.
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#getting-supported-list-of-input-formats).
	///
	/// # Errors
	///
	/// Could error if the encode GUID is invalid
	/// or we run out of memory.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::{
	/// #     sys::nvEncodeAPI::{NV_ENC_BUFFER_FORMAT, NV_ENC_CODEC_H264_GUID},
	/// #     Encoder,
	/// # };
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	///
	/// //* Check if H.264 encoding is supported. *//
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&NV_ENC_CODEC_H264_GUID));
	///
	/// let input_guids = encoder
	///     .get_supported_input_formats(NV_ENC_CODEC_H264_GUID)
	///     .unwrap();
	/// // Confirm that H.264 supports the `ARGB10` format on this machine.
	/// assert!(input_guids.contains(&NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB10));
	/// ```
	pub fn get_supported_input_formats(&self, encode_guid: GUID) -> Result<Vec<NV_ENC_BUFFER_FORMAT>, EncodeError> {
		// Query the number of supported input formats.
		let mut format_count = 0;
		unsafe { (self.api.get_input_format_count)(self.ptr, encode_guid, &mut format_count) }.result(self)?;
		// Get the supported input formats.
		let mut supported_input_formats =
			vec![NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_UNDEFINED; format_count as usize];
		let mut actual_count = 0;
		unsafe {
			(self.api.get_input_formats)(
				self.ptr,
				encode_guid,
				supported_input_formats.as_mut_ptr(),
				format_count,
				&mut actual_count,
			)
		}
		.result(self)?;
		supported_input_formats.truncate(actual_count as usize);
		Ok(supported_input_formats)
	}

	/// Get the preset config struct from the given codec GUID, preset GUID,
	/// and tuning info.
	///
	/// You should use this function to generate a preset config for the
	/// encoder session if you want to modify the preset further.
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#selecting-encoder-preset-configuration)
	///
	/// # Errors
	///
	/// Could error if `encode_guid` or `preset_guid` is invalid,
	/// if `tuning_info` is set to
	/// [`NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_UNDEFINED`] or
	/// [`NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_COUNT`],
	/// or if we run out of memory.
	///
	/// # Examples
	///
	/// ```no_run
	/// # use cudarc::driver::CudaContext;
	/// # use moq_nvenc::{
	/// #     sys::nvEncodeAPI::{
	/// #         NV_ENC_CODEC_H264_GUID,
	/// #         NV_ENC_PRESET_P1_GUID,
	/// #         NV_ENC_TUNING_INFO,
	/// #     },
	/// #     Encoder,
	/// # };
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	///
	/// //* Check if H.264 encoding and the P1 preset (highest performance) are supported. *//
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&NV_ENC_CODEC_H264_GUID));
	/// # let preset_guids = encoder.get_preset_guids(NV_ENC_CODEC_H264_GUID).unwrap();
	/// # assert!(preset_guids.contains(&NV_ENC_PRESET_P1_GUID));
	///
	/// // Create the preset config.
	/// let _preset_config = encoder
	///     .get_preset_config(
	///         NV_ENC_CODEC_H264_GUID,
	///         NV_ENC_PRESET_P1_GUID,
	///         NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
	///     )
	///     .unwrap();
	/// ```
	pub fn get_preset_config(
		&self,
		encode_guid: GUID,
		preset_guid: GUID,
		tuning_info: NV_ENC_TUNING_INFO,
	) -> Result<NV_ENC_PRESET_CONFIG, EncodeError> {
		let mut preset_config = NV_ENC_PRESET_CONFIG {
			version: NV_ENC_PRESET_CONFIG_VER,
			presetCfg: NV_ENC_CONFIG {
				version: NV_ENC_CONFIG_VER,
				..Default::default()
			},
			..Default::default()
		};
		unsafe {
			(self.api.get_encode_preset_config_ex)(self.ptr, encode_guid, preset_guid, tuning_info, &mut preset_config)
		}
		.result(self)?;
		Ok(preset_config)
	}

	/// Initialize an encoder session with the given configuration.
	///
	/// You must do this before you can encode a picture.
	/// You should use the [`EncoderInitParams`] builder
	/// via [`EncoderInitParams::new`].
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#initializing-the-hardware-encoder-session).
	///
	/// # Errors
	///
	/// Could error if the `initialize_params` are invalid
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
	/// #     },
	/// #     Encoder, EncoderInitParams
	/// # };
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	///
	/// //* Check if `NV_ENC_CODEC_H264_GUID` is supported. *//
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&NV_ENC_CODEC_H264_GUID));
	///
	/// // Initialize the encoder session.
	/// let _session = encoder
	///     .start_session(
	///         NV_ENC_BUFFER_FORMAT_ARGB,
	///         EncoderInitParams::new(NV_ENC_CODEC_H264_GUID, 1920, 1080),
	///     )
	///     .unwrap();
	/// ```
	pub fn start_session(
		self,
		buffer_format: NV_ENC_BUFFER_FORMAT,
		mut initialize_params: EncoderInitParams,
	) -> Result<Session, EncodeError> {
		let mut config = initialize_params.config.take();
		initialize_params.param.encodeConfig = config.as_deref_mut().map_or(std::ptr::null_mut(), std::ptr::from_mut);
		let initialize_params = &mut initialize_params.param;
		let width = initialize_params.encodeWidth;
		let height = initialize_params.encodeHeight;
		unsafe { (self.api.initialize_encoder)(self.ptr, initialize_params) }.result(&self)?;

		// The box keeps its address when the session moves and remains owned for
		// reconfiguration, so `init.encodeConfig` never points into caller storage.
		let mut init = *initialize_params;
		init.encodeConfig = match config.as_mut() {
			Some(config) => std::ptr::from_mut::<NV_ENC_CONFIG>(&mut **config),
			None => std::ptr::null_mut(),
		};

		Ok(Session {
			encoder: Arc::new(self),
			width,
			height,
			buffer_format,
			encode_guid: initialize_params.encodeGUID,
			init,
			config,
		})
	}
}

/// Open an encode session, destroying whatever a failed open left behind as
/// NVENC requires. A failed destroy is attached to the open error, not
/// reported in its place.
fn open_session(
	open: impl FnOnce(&mut *mut c_void) -> Result<(), EncodeError>,
	destroy: impl FnOnce(*mut c_void) -> Result<(), EncodeError>,
) -> Result<*mut c_void, EncodeError> {
	let mut encoder = ptr::null_mut();
	let Err(primary) = open(&mut encoder) else {
		return Ok(encoder);
	};
	if encoder.is_null() {
		return Err(primary);
	}
	match destroy(encoder) {
		Ok(()) => Err(primary),
		Err(cleanup) => Err(primary.with_cleanup(cleanup)),
	}
}

/// A safe wrapper for [`NV_ENC_INITIALIZE_PARAMS`], which is the encoder
/// initialize parameter.
pub struct EncoderInitParams {
	param: NV_ENC_INITIALIZE_PARAMS,
	config: Option<Box<NV_ENC_CONFIG>>,
}

impl std::fmt::Debug for EncoderInitParams {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("EncoderInitParams")
			.field("width", &self.param.encodeWidth)
			.field("height", &self.param.encodeHeight)
			.field("has_config", &self.config.is_some())
			.finish_non_exhaustive()
	}
}

impl EncoderInitParams {
	/// Create a new builder for [`EncoderInitParams`], which is a wrapper for
	/// [`NV_ENC_INITIALIZE_PARAMS`].
	#[must_use]
	pub fn new(encode_guid: GUID, width: u32, height: u32) -> Self {
		let param = NV_ENC_INITIALIZE_PARAMS {
			version: NV_ENC_INITIALIZE_PARAMS_VER,
			encodeGUID: encode_guid,
			encodeWidth: width,
			encodeHeight: height,
			..Default::default()
		};
		Self { param, config: None }
	}

	/// Specifies the preset for encoding. If the preset GUID is set then
	/// the preset configuration will be applied before any other parameter.
	pub fn preset_guid(&mut self, preset_guid: GUID) -> &mut Self {
		self.param.presetGUID = preset_guid;
		self
	}

	/// Tuning Info of NVENC encoding(`tuning_info` is not applicable to H264
	/// and HEVC meonly mode).
	pub fn tuning_info(&mut self, tuning_info: NV_ENC_TUNING_INFO) -> &mut Self {
		self.param.tuningInfo = tuning_info;
		self
	}

	/// Specifies the advanced codec specific structure. If client has sent a
	/// valid codec config structure, it will override parameters set by the
	/// [`EncoderInitParams::preset_guid`].
	///
	/// The client can query the interface for codec-specific parameters
	/// using [`Encoder::get_preset_config`](super::encoder::Encoder::get_preset_config).
	/// It can then modify (if required) some of the codec config parameters and
	/// send down a custom config structure using this method. Even in this
	/// case the client is recommended to pass the same preset GUID it has
	/// used to get the config.
	///
	/// # Safety
	///
	/// Any pointers embedded in `encode_config` must remain valid until the
	/// encoder session is dropped. Prefer a config returned by
	/// [`Encoder::get_preset_config`], whose reserved pointers are null.
	pub unsafe fn encode_config(&mut self, encode_config: NV_ENC_CONFIG) -> &mut Self {
		self.config = Some(Box::new(encode_config));
		self.param.encodeConfig = self
			.config
			.as_deref_mut()
			.map_or(std::ptr::null_mut(), std::ptr::from_mut);
		self
	}

	/// Specifies the display aspect ratio (H264/HEVC) or the render
	/// width/height (AV1).
	pub fn display_aspect_ratio(&mut self, width: u32, height: u32) -> &mut Self {
		self.param.darWidth = width;
		self.param.darHeight = height;
		self
	}

	/// Specifies the framerate in frames per second as a fraction
	/// `numerator / denominator`.
	pub fn framerate(&mut self, numerator: u32, denominator: u32) -> &mut Self {
		self.param.frameRateNum = numerator;
		self.param.frameRateDen = denominator;
		self
	}

	/// Enable the Picture Type Decision to be taken by the
	/// `NvEncodeAPI` interface.
	pub fn enable_picture_type_decision(&mut self) -> &mut Self {
		self.param.enablePTD = 1;
		self
	}
}

#[cfg(test)]
mod tests {
	use std::cell::RefCell;

	use super::*;
	use crate::safe::result::ErrorKind;

	fn handle() -> *mut c_void {
		ptr::NonNull::<u8>::dangling().as_ptr().cast()
	}

	fn fail(kind: ErrorKind) -> Result<(), EncodeError> {
		Err(EncodeError::new(kind, None))
	}

	#[test]
	fn failed_open_destroys_the_partial_session() {
		let destroyed = RefCell::new(Vec::new());
		let error = open_session(
			|encoder| {
				*encoder = handle();
				fail(ErrorKind::InvalidVersion)
			},
			|encoder| {
				destroyed.borrow_mut().push(encoder);
				Ok(())
			},
		)
		.expect_err("open failed");
		assert_eq!(error.kind(), ErrorKind::InvalidVersion);
		assert!(error.cleanup().is_none());
		assert_eq!(*destroyed.borrow(), [handle()]);
	}

	#[test]
	fn failed_destroy_keeps_the_open_error() {
		let error = open_session(
			|encoder| {
				*encoder = handle();
				fail(ErrorKind::NoEncodeDevice)
			},
			|_| fail(ErrorKind::InvalidPtr),
		)
		.expect_err("open failed");
		assert_eq!(error.kind(), ErrorKind::NoEncodeDevice);
		assert_eq!(error.cleanup().map(EncodeError::kind), Some(ErrorKind::InvalidPtr));
	}

	#[test]
	fn failed_open_without_a_handle_destroys_nothing() {
		let error = open_session(|_| fail(ErrorKind::OutOfMemory), |_| panic!("destroyed a null session"))
			.expect_err("open failed");
		assert_eq!(error.kind(), ErrorKind::OutOfMemory);
	}

	#[test]
	fn successful_open_destroys_nothing() {
		let encoder = open_session(
			|encoder| {
				*encoder = handle();
				Ok(())
			},
			|_| panic!("destroyed a live session"),
		)
		.unwrap();
		assert_eq!(encoder, handle());
	}
}

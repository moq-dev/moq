//! Defines [`Session`] which represents an ongoing encoder session.
//!
//! You need to start a session using [`Encoder::start_session`] before
//! you can initialize input or output buffers, and before you can encode
//! frames. The [`Session`] also stores some information such as the encode
//! width and height so that you do not have to keep repeating it each time.

use std::fmt::Debug;

use super::{
	api::ENCODE_API,
	encoder::Encoder,
	result::{EncodeError, ErrorKind},
};
use crate::{
	sys::nvEncodeAPI::{
		GUID, NV_ENC_BUFFER_FORMAT, NV_ENC_CODEC_AV1_GUID, NV_ENC_CODEC_H264_GUID, NV_ENC_CODEC_HEVC_GUID,
		NV_ENC_CODEC_PIC_PARAMS, NV_ENC_CONFIG, NV_ENC_INITIALIZE_PARAMS, NV_ENC_PIC_FLAGS, NV_ENC_PIC_PARAMS,
		NV_ENC_PIC_PARAMS_AV1, NV_ENC_PIC_PARAMS_H264, NV_ENC_PIC_PARAMS_HEVC, NV_ENC_PIC_PARAMS_VER,
		NV_ENC_PIC_STRUCT, NV_ENC_PIC_TYPE, NV_ENC_RECONFIGURE_PARAMS, NV_ENC_RECONFIGURE_PARAMS_VER,
	},
	EncoderInput, EncoderOutput,
};

/// An encoding session to create input/output buffers and encode frames.
///
/// You need to call [`Encoder::start_session`] before you can
/// encode frames using the session. On drop, the session will automatically
/// send an empty EOS frame to flush the encoder.
pub struct Session {
	pub(crate) encoder: Encoder,
	pub(crate) width: u32,
	pub(crate) height: u32,
	pub(crate) buffer_format: NV_ENC_BUFFER_FORMAT,
	pub(crate) encode_guid: GUID,

	/// The parameters the session was initialized with, retained so
	/// [`reconfigure`](Self::reconfigure) can resubmit them with one field
	/// changed: `NvEncReconfigureEncoder` takes the *whole* init params, not a
	/// delta.
	pub(crate) init: NV_ENC_INITIALIZE_PARAMS,

	/// Owned copy of the encode config `init.encodeConfig` points at. Boxed so
	/// the pointer survives moving the `Session`, and owned because the caller's
	/// config is borrowed only for the duration of `start_session`. `None` when
	/// the caller supplied no config (NVENC then uses preset defaults, which we
	/// can't resubmit because we never saw them).
	pub(crate) config: Option<Box<NV_ENC_CONFIG>>,
}

// Hand-written because the retained bindgen params are plain C structs with no
// `Debug`, and dumping hundreds of codec fields would drown the useful ones.
impl Debug for Session {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Session")
			.field("encoder", &self.encoder)
			.field("width", &self.width)
			.field("height", &self.height)
			.field("buffer_format", &self.buffer_format)
			.field("encode_guid", &self.encode_guid)
			.finish_non_exhaustive()
	}
}

impl Session {
	/// Get the encoder used for this session.
	///
	/// This might be useful if you want to use some of
	/// the functions on [`Encoder`].
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
	/// //* Create encoder. *//
	/// # let cuda_ctx = CudaContext::new(0).unwrap();
	/// # let encoder = Encoder::initialize_with_cuda(cuda_ctx).unwrap();
	///
	/// //* Set `encode_guid` and check that H.264 encoding is supported. *//
	/// # let encode_guid = NV_ENC_CODEC_H264_GUID;
	/// # let encode_guids = encoder.get_encode_guids().unwrap();
	/// # assert!(encode_guids.contains(&encode_guid));
	///
	/// let session = encoder
	///     .start_session(
	///         NV_ENC_BUFFER_FORMAT_ARGB,
	///         EncoderInitParams::new(encode_guid, 1920, 1080),
	///     )
	///     .unwrap();
	/// // We can still use the encoder like this:
	/// let _input_formats = session
	///     .get_encoder()
	///     .get_supported_input_formats(encode_guid);
	/// ```
	#[must_use]
	pub fn get_encoder(&self) -> &Encoder {
		&self.encoder
	}

	/// Change the average bitrate (bits per second) of the running session,
	/// taking effect from roughly the next frame.
	///
	/// The encoder keeps running: no IDR is forced and no state is reset, so
	/// this is safe to call as often as a congestion controller updates. That is
	/// the point of the narrow signature. `NvEncReconfigureEncoder` can also
	/// change the resolution and reset the encoder, but those need a new session
	/// (the width/height this `Session` caches would go stale) and would emit
	/// exactly the keyframe burst a congested link cannot absorb.
	///
	/// # Errors
	///
	/// Returns [`ErrorKind::InvalidParam`] when the session was started without
	/// an encode config, since there is then no config to resubmit. Otherwise
	/// returns whatever `NvEncReconfigureEncoder` reports, e.g.
	/// [`ErrorKind::UnsupportedParam`] if the driver rejects the rate change.
	pub fn reconfigure(&mut self, bitrate: u32) -> Result<(), EncodeError> {
		let Some(config) = self.config.as_mut() else {
			return Err(EncodeError::new(
				ErrorKind::InvalidParam,
				Some("session was started without an encode config to reconfigure".into()),
			));
		};

		config.rcParams.averageBitRate = bitrate;
		// The caller's original pointer died with `start_session`; re-point at
		// our owned copy in case the `Session` moved since.
		self.init.encodeConfig = std::ptr::from_mut::<NV_ENC_CONFIG>(&mut **config);

		let mut params = NV_ENC_RECONFIGURE_PARAMS {
			version: NV_ENC_RECONFIGURE_PARAMS_VER,
			reInitEncodeParams: self.init,
			..unsafe { std::mem::zeroed() }
		};
		// Leave resetEncoder and forceIDR clear: retune in place, no keyframe.
		params.set_resetEncoder(0);
		params.set_forceIDR(0);

		unsafe { (ENCODE_API.reconfigure_encoder)(self.encoder.ptr, &mut params) }.result(&self.encoder)
	}

	/// Encode a frame.
	///
	/// See [NVIDIA docs](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html#submitting-input-frame-for-encoding).
	///
	/// # Errors
	///
	/// Could error if the encode picture parameters were invalid or otherwise
	/// incorrect, or if we run out memory.
	///
	/// There are two recoverable errors:
	/// - If this returns an error with
	///   [`ErrorKind::EncoderBusy`](super::ErrorKind::EncoderBusy) then you
	///   should retry after a few milliseconds.
	/// - If this returns an error with
	///   [`ErrorKind::NeedMoreInput`](super::ErrorKind::NeedMoreInput), the
	///   client should not lock the output bitstream yet. They should continue
	///   encoding until this function returns `Ok`, and then lock the
	///   bitstreams in the order in which they were originally used.
	///
	/// # Panics
	///
	/// Panics if codec specific parameters are provided for a different codec
	/// than the one used in the session.
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
	/// #     Encoder, EncoderInitParams,
	/// #     EncodePictureParams
	/// # };
	/// # const WIDTH: u32 = 1920;
	/// # const HEIGHT: u32 = 1080;
	/// # const DATA_LEN: usize = (WIDTH * HEIGHT * 4) as usize;
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
	/// // Begin encoder session.
	/// let mut initialize_params = EncoderInitParams::new(encode_guid, WIDTH, HEIGHT);
	/// initialize_params.display_aspect_ratio(16, 9)
	///     .framerate(30, 1)
	///     .enable_picture_type_decision();
	/// let session = encoder.start_session(
	///     buffer_format,
	///     initialize_params,
	/// ).unwrap();
	///
	/// //* Create input and output buffers. *//
	/// # let mut input_buffer = session
	/// #     .create_input_buffer()
	/// #     .unwrap();
	/// # let mut output_bitstream = session.create_output_bitstream().unwrap();
	///
	/// // Encode frame.
	/// unsafe { input_buffer.lock().unwrap().write(&[0; DATA_LEN]) };
	/// session
	///     .encode_picture(
	///         &mut input_buffer,
	///         &mut output_bitstream,
	///         // Optional picture parameters
	///         EncodePictureParams {
	///             input_timestamp: 42,
	///             ..Default::default()
	///         }
	///     )
	///     .unwrap();
	/// # // TODO: check that output is correct.
	/// let _data = output_bitstream.lock().unwrap().data();
	/// ```
	pub fn encode_picture<I: EncoderInput, O: EncoderOutput>(
		&self,
		input_buffer: &mut I,
		output_bitstream: &mut O,
		params: EncodePictureParams,
	) -> Result<(), EncodeError> {
		if let Some(codec_params) = &params.codec_params {
			assert_eq!(
				codec_params.get_codec_guid(),
				self.encode_guid,
				"The provided codec specific params must match the codec used"
			);
		}
		let mut encode_pic_params = NV_ENC_PIC_PARAMS {
			version: NV_ENC_PIC_PARAMS_VER,
			inputWidth: self.width,
			inputHeight: self.height,
			inputPitch: input_buffer.pitch(),
			inputBuffer: input_buffer.handle(),
			outputBitstream: output_bitstream.handle(),
			bufferFmt: self.buffer_format,
			pictureStruct: NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME,
			inputTimeStamp: params.input_timestamp,
			codecPicParams: params.codec_params.map(Into::into).unwrap_or_default(),
			pictureType: params.picture_type,
			// Force an IDR at this frame regardless of the encoder's own GOP /
			// picture-type decision. Unlike `pictureType` (honored only when
			// picture-type decision is disabled), `NV_ENC_PIC_FLAG_FORCEIDR`
			// applies with it enabled, which is how you request an out-of-cadence
			// keyframe.
			encodePicFlags: if params.force_idr {
				NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_FORCEIDR as u32
			} else {
				0
			},
			..Default::default()
		};
		unsafe { (ENCODE_API.encode_picture)(self.encoder.ptr, &mut encode_pic_params) }.result(&self.encoder)
	}

	/// Send an EOS notifications to flush the encoder.
	///
	/// This function is called automatically on drop, but if you wish to
	/// get the data after flushing, you should call this function yourself.
	///
	/// # Errors
	///
	/// Could error if we run out of memory.
	///
	/// If this returns an error with
	/// [`ErrorKind::EncoderBusy`](super::ErrorKind::EncoderBusy) then you
	/// should retry after a few milliseconds.
	pub fn end_of_stream(&self) -> Result<(), EncodeError> {
		let mut encode_pic_params = NV_ENC_PIC_PARAMS::end_of_stream();
		unsafe { (ENCODE_API.encode_picture)(self.encoder.ptr, &mut encode_pic_params) }.result(&self.encoder)
	}
}

/// Send an EOS notifications on drop to flush the encoder.
impl Drop for Session {
	fn drop(&mut self) {
		if !std::thread::panicking() {
			self.end_of_stream().expect("The encoder should not be busy.");
		}
	}
}

/// Optional parameters for [`Session::encode_picture`].
#[allow(missing_debug_implementations)] // CodecPictureParams doesn't implement Debug
pub struct EncodePictureParams {
	/// Opaque data used for identifying the corresponding encoded frame
	pub input_timestamp: u64,
	/// The picture type to use, if picture type decision is disabled in the
	/// encoder
	pub picture_type: NV_ENC_PIC_TYPE,
	/// Force this frame to be an IDR (`NV_ENC_PIC_FLAG_FORCEIDR`). Works with
	/// picture-type decision enabled, so it is the way to request an
	/// out-of-cadence keyframe.
	pub force_idr: bool,
	/// Codec-specific parameters
	pub codec_params: Option<CodecPictureParams>,
}

impl Default for EncodePictureParams {
	fn default() -> Self {
		Self {
			input_timestamp: 0,
			picture_type: NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_UNKNOWN,
			force_idr: false,
			codec_params: None,
		}
	}
}

/// Codec specific picture parameters
#[allow(missing_debug_implementations)] // NV_ENC_PIC_PARAMS_H264 contains a union, thus doesn't derive Debug
pub enum CodecPictureParams {
	/// Parameters for H.264
	H264(NV_ENC_PIC_PARAMS_H264),
	/// Parameters for HEVC or H.265
	Hevc(NV_ENC_PIC_PARAMS_HEVC),
	/// Parameters for AV1
	Av1(NV_ENC_PIC_PARAMS_AV1),
}

impl CodecPictureParams {
	/// Returns the GUID representing the codec for which the parameters are
	/// specified.
	#[must_use]
	pub fn get_codec_guid(&self) -> GUID {
		match self {
			Self::H264(_) => NV_ENC_CODEC_H264_GUID,
			Self::Hevc(_) => NV_ENC_CODEC_HEVC_GUID,
			Self::Av1(_) => NV_ENC_CODEC_AV1_GUID,
		}
	}
}

impl From<CodecPictureParams> for NV_ENC_CODEC_PIC_PARAMS {
	fn from(value: CodecPictureParams) -> Self {
		match value {
			CodecPictureParams::H264(params) => Self { h264PicParams: params },
			CodecPictureParams::Hevc(params) => Self { hevcPicParams: params },
			CodecPictureParams::Av1(params) => Self { av1PicParams: params },
		}
	}
}

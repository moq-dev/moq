//! Defines [`Session`] which represents an ongoing encoder session.
//!
//! You need to start a session using [`Encoder::start_session`] before
//! you can initialize input or output buffers, and before you can encode
//! frames. The [`Session`] also stores some information such as the encode
//! width and height so that you do not have to keep repeating it each time.

use std::{fmt::Debug, sync::Arc};

use super::{
	encoder::Encoder,
	result::{EncodeError, ErrorKind},
};
use crate::{
	sys::nvEncodeAPI::{
		GUID, NV_ENC_BUFFER_FORMAT, NV_ENC_CONFIG, NV_ENC_INITIALIZE_PARAMS, NV_ENC_PIC_FLAGS, NV_ENC_PIC_PARAMS,
		NV_ENC_PIC_PARAMS_VER, NV_ENC_PIC_STRUCT, NV_ENC_RECONFIGURE_PARAMS, NV_ENC_RECONFIGURE_PARAMS_VER,
	},
	Bitstream, EncoderInput,
};

/// An encoding session to create input/output buffers and encode frames.
///
/// You need to call [`Encoder::start_session`] before you can
/// encode frames using the session. On drop, the session will automatically
/// send an empty EOS frame to flush the encoder.
pub struct Session {
	pub(crate) encoder: Arc<Encoder>,
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
	/// an encode config, since there is then no config to resubmit, when
	/// `bitrate` is zero, or when the proportionally scaled VBV overflows.
	/// Otherwise returns whatever `NvEncReconfigureEncoder` reports, e.g.
	/// [`ErrorKind::UnsupportedParam`] if the driver rejects the rate change.
	/// After any error the session keeps its last accepted rate settings.
	pub fn reconfigure(&mut self, bitrate: u32) -> Result<(), EncodeError> {
		let Some(config) = self.config.as_mut() else {
			return Err(EncodeError::new(
				ErrorKind::InvalidParam,
				Some("session was started without an encode config to reconfigure".into()),
			));
		};
		debug_assert_eq!(
			self.init.encodeConfig,
			std::ptr::from_mut::<NV_ENC_CONFIG>(&mut **config),
			"init.encodeConfig must point at our owned copy, not the caller's dead one"
		);

		let encoder = &self.encoder;
		retune(&self.init, config, bitrate, |params| {
			unsafe { (encoder.api.reconfigure_encoder)(encoder.ptr, params) }.result(encoder)
		})
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
	/// An encoder-busy result is returned as an error so the caller can retry.
	/// A need-more-input result is instead represented by the returned
	/// [`Submission`], which retains both buffers until completion. The facade
	/// does not drive frames the driver holds back, so configure the session
	/// without B-frames or lookahead (`frameIntervalP = 1` and low-latency
	/// tuning, as `moq-video` does): the driver refuses to lock a held frame's
	/// output, and [`Submission::finish`] fails.
	///
	/// Safe code cannot release the input while it is in flight because the
	/// submission owns it:
	///
	/// ```compile_fail
	/// # use moq_nvenc::{Bitstream, Buffer, EncodePictureParams, Session};
	/// fn submit(session: &Session, input: Buffer, output: Bitstream) {
	///     let pending = session.encode_picture(input, output, EncodePictureParams::default()).unwrap();
	///     drop(input);
	///     drop(pending);
	/// }
	/// ```
	///
	/// There is one recoverable error:
	/// - If this returns an error with
	///   [`ErrorKind::EncoderBusy`](super::ErrorKind::EncoderBusy) then you
	///   should retry after a few milliseconds.
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
	/// # let output_bitstream = session.create_output_bitstream().unwrap();
	///
	/// // Encode frame.
	/// unsafe { input_buffer.lock().unwrap().write(&[0; DATA_LEN]) };
	/// let submission = session
	///     .encode_picture(
	///         input_buffer,
	///         output_bitstream,
	///         // Optional picture parameters
	///         EncodePictureParams {
	///             input_timestamp: 42,
	///             ..Default::default()
	///         }
	///     )
	///     .unwrap();
	/// let (data, _input_buffer, _output_bitstream) = submission.finish().unwrap();
	/// ```
	pub fn encode_picture<I: EncoderInput>(
		&self,
		mut input_buffer: I,
		output_bitstream: Bitstream,
		params: EncodePictureParams,
	) -> Result<Submission<I>, EncodeError> {
		if !same_session(input_buffer.encoder(), &output_bitstream.encoder, &self.encoder) {
			return Err(EncodeError::new(
				ErrorKind::InvalidParam,
				Some("input and output must belong to this session".into()),
			));
		}
		let mut encode_pic_params = NV_ENC_PIC_PARAMS {
			version: NV_ENC_PIC_PARAMS_VER,
			inputWidth: self.width,
			inputHeight: self.height,
			inputPitch: input_buffer.pitch(),
			inputBuffer: input_buffer.handle(),
			outputBitstream: output_bitstream.ptr,
			bufferFmt: self.buffer_format,
			pictureStruct: NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME,
			inputTimeStamp: params.input_timestamp,
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
		let result = unsafe { (self.encoder.api.encode_picture)(self.encoder.ptr, &mut encode_pic_params) }
			.result(&self.encoder);
		match result {
			Ok(()) => Ok(Submission::new(input_buffer, output_bitstream)),
			Err(error) if error.kind() == ErrorKind::NeedMoreInput => {
				Ok(Submission::new(input_buffer, output_bitstream))
			}
			Err(error) => Err(error),
		}
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
		unsafe { (self.encoder.api.encode_picture)(self.encoder.ptr, &mut encode_pic_params) }.result(&self.encoder)
	}
}

/// Send an EOS notifications on drop to flush the encoder.
impl Drop for Session {
	fn drop(&mut self) {
		let _ = self.end_of_stream();
	}
}

/// Optional parameters for [`Session::encode_picture`].
#[derive(Debug, Default)]
pub struct EncodePictureParams {
	/// Opaque data used for identifying the corresponding encoded frame
	pub input_timestamp: u64,
	/// Force this frame to be an IDR (`NV_ENC_PIC_FLAG_FORCEIDR`). Works with
	/// picture-type decision enabled, so it is the way to request an
	/// out-of-cadence keyframe.
	pub force_idr: bool,
}

/// Resources retained until NVENC has completed a submitted picture.
#[derive(Debug)]
#[must_use = "dropping a submission waits for completion before releasing its resources"]
pub struct Submission<I> {
	pending: Pending<SdkDriver<I>>,
}

impl<I> Submission<I> {
	fn new(input: I, output: Bitstream) -> Self
	where
		I: EncoderInput,
	{
		let driver = SdkDriver {
			abandon_input: I::abandon,
		};
		Self {
			pending: Pending::new(driver, input, output),
		}
	}

	/// Wait for completion, copy the encoded bytes, and return reusable buffers.
	///
	/// # Errors
	///
	/// Returns the driver's error when the output cannot be locked. Both
	/// buffers are then abandoned, since the driver may still be using them.
	pub fn finish(mut self) -> Result<(Vec<u8>, I, Bitstream), EncodeError> {
		self.pending.finish()
	}
}

fn same_session<T>(input: &Arc<T>, output: &Arc<T>, session: &Arc<T>) -> bool {
	Arc::ptr_eq(input, session) && Arc::ptr_eq(output, session)
}

/// Submit `config` retuned to `bitrate` and commit it only once `submit`
/// accepts, so a rejected change cannot skew the basis of the next one.
fn retune(
	init: &NV_ENC_INITIALIZE_PARAMS,
	config: &mut NV_ENC_CONFIG,
	bitrate: u32,
	submit: impl FnOnce(&mut NV_ENC_RECONFIGURE_PARAMS) -> Result<(), EncodeError>,
) -> Result<(), EncodeError> {
	let invalid = |reason: &str| EncodeError::new(ErrorKind::InvalidParam, Some(reason.into()));
	// A zero rate would also zero a proportional VBV, which no later rate could scale back up.
	if bitrate == 0 {
		return Err(invalid("bitrate must be nonzero"));
	}

	let mut candidate = *config;
	let rc = &mut candidate.rcParams;
	// Keep a caller-sized VBV proportional to the rate, so a buffer sized to
	// one frame at open stays one frame: left alone it would loosen the
	// keyframe cap as the bitrate falls, right when the link can least afford it.
	if rc.vbvBufferSize != 0 && rc.averageBitRate != 0 {
		let basis = u64::from(rc.averageBitRate);
		let scale = |v: u32| {
			u32::try_from(u64::from(v) * u64::from(bitrate) / basis).map_err(|_| invalid("scaled VBV exceeds u32"))
		};
		rc.vbvBufferSize = scale(rc.vbvBufferSize)?;
		rc.vbvInitialDelay = scale(rc.vbvInitialDelay)?;
	}
	rc.averageBitRate = bitrate;

	// NVENC copies the config during the call, so pointing it at the local
	// candidate is sound; `init` keeps pointing at the committed copy.
	let mut params = NV_ENC_RECONFIGURE_PARAMS {
		version: NV_ENC_RECONFIGURE_PARAMS_VER,
		reInitEncodeParams: NV_ENC_INITIALIZE_PARAMS {
			encodeConfig: &mut candidate,
			..*init
		},
		..unsafe { std::mem::zeroed() }
	};
	// Leave resetEncoder and forceIDR clear: retune in place, no keyframe.
	params.set_resetEncoder(0);
	params.set_forceIDR(0);

	submit(&mut params)?;
	*config = candidate;
	Ok(())
}

trait CompletionDriver {
	type Input;
	type Output;

	fn wait(&self, output: &mut Self::Output) -> Result<Vec<u8>, EncodeError>;

	/// Give up both buffers after a failed wait.
	fn abandon(&self, input: Self::Input, output: Self::Output);
}

#[derive(Debug)]
struct SdkDriver<I> {
	// A function rather than an `EncoderInput` bound, which `Submission` would
	// have to repeat in its public signature.
	abandon_input: fn(I),
}

impl<I> CompletionDriver for SdkDriver<I> {
	type Input = I;
	type Output = Bitstream;

	fn wait(&self, output: &mut Self::Output) -> Result<Vec<u8>, EncodeError> {
		Ok(output.lock()?.data().to_vec())
	}

	fn abandon(&self, input: I, output: Bitstream) {
		(self.abandon_input)(input);
		output.abandon();
	}
}

#[derive(Debug)]
struct Pending<D: CompletionDriver> {
	driver: D,
	/// `None` once finished or abandoned.
	buffers: Option<(D::Input, D::Output)>,
}

impl<D: CompletionDriver> Pending<D> {
	fn new(driver: D, input: D::Input, output: D::Output) -> Self {
		Self {
			driver,
			buffers: Some((input, output)),
		}
	}

	fn finish(&mut self) -> Result<(Vec<u8>, D::Input, D::Output), EncodeError> {
		let (input, mut output) = self.buffers.take().expect("submission buffers");
		// Wait without sending end-of-stream: flushing here would end the
		// session, while the caller may still submit further frames.
		match self.driver.wait(&mut output) {
			Ok(data) => Ok((data, input, output)),
			Err(error) => {
				// A failed wait cannot prove the driver released either buffer, so
				// leak them rather than permit a use-after-free. Their encoder
				// reference still goes: a session left open at exit deadlocks the
				// driver's own exit handler, hanging the process.
				self.driver.abandon(input, output);
				Err(error)
			}
		}
	}
}

impl<D: CompletionDriver> Drop for Pending<D> {
	fn drop(&mut self) {
		if self.buffers.is_some() {
			let _ = self.finish();
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::{Arc, Mutex};

	use cudarc::driver::CudaContext;

	use super::*;
	use crate::{
		sys::nvEncodeAPI::{
			NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12, NV_ENC_CODEC_H264_GUID, NV_ENC_PRESET_P7_GUID,
			NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_HIGH_QUALITY,
		},
		EncoderInitParams,
	};

	type Events = Arc<Mutex<Vec<&'static str>>>;

	#[derive(Debug)]
	struct Resource(&'static str, Events);

	impl Drop for Resource {
		fn drop(&mut self) {
			self.1.lock().unwrap().push(self.0);
		}
	}

	#[derive(Debug)]
	struct FakeDriver {
		events: Events,
		fail: bool,
	}

	impl CompletionDriver for FakeDriver {
		type Input = Resource;
		type Output = Resource;

		fn wait(&self, _: &mut Self::Output) -> Result<Vec<u8>, EncodeError> {
			self.events.lock().unwrap().push("wait");
			if self.fail {
				return Err(EncodeError::new(ErrorKind::InvalidParam, None));
			}
			Ok(vec![1, 2, 3])
		}

		fn abandon(&self, input: Resource, output: Resource) {
			self.events.lock().unwrap().push("abandon");
			std::mem::forget((input, output));
		}
	}

	fn pending(events: &Events, fail: bool) -> Pending<FakeDriver> {
		let driver = FakeDriver {
			events: events.clone(),
			fail,
		};
		Pending::new(
			driver,
			Resource("input", events.clone()),
			Resource("output", events.clone()),
		)
	}

	#[test]
	fn delayed_completion_retains_resources_until_wait() {
		let events = Events::default();
		let (data, input, output) = pending(&events, false).finish().unwrap();
		assert_eq!(data, [1, 2, 3]);
		assert_eq!(*events.lock().unwrap(), ["wait"]);
		drop((input, output));
		assert_eq!(*events.lock().unwrap(), ["wait", "input", "output"]);
	}

	#[test]
	fn cancellation_completes_before_releasing_resources() {
		let events = Events::default();
		drop(pending(&events, false));
		assert_eq!(*events.lock().unwrap(), ["wait", "input", "output"]);
	}

	fn rate_config(bitrate: u32, vbv: u32) -> NV_ENC_CONFIG {
		let mut config = NV_ENC_CONFIG::default();
		config.rcParams.averageBitRate = bitrate;
		config.rcParams.vbvBufferSize = vbv;
		config.rcParams.vbvInitialDelay = vbv;
		config
	}

	/// The (average, VBV size, VBV delay) a reconfigure submitted.
	fn submitted(params: &NV_ENC_RECONFIGURE_PARAMS) -> (u32, u32, u32) {
		let rc = unsafe { &(*params.reInitEncodeParams.encodeConfig).rcParams };
		(rc.averageBitRate, rc.vbvBufferSize, rc.vbvInitialDelay)
	}

	fn rates(config: &NV_ENC_CONFIG) -> (u32, u32, u32) {
		let rc = &config.rcParams;
		(rc.averageBitRate, rc.vbvBufferSize, rc.vbvInitialDelay)
	}

	#[test]
	fn rejected_rate_change_keeps_the_last_accepted_basis() {
		let init = NV_ENC_INITIALIZE_PARAMS {
			encodeWidth: 1280,
			..Default::default()
		};
		let mut config = rate_config(1_000_000, 100_000);

		let error = retune(&init, &mut config, 500_000, |params| {
			assert_eq!(submitted(params), (500_000, 50_000, 50_000));
			assert_eq!(params.reInitEncodeParams.encodeWidth, 1280);
			Err(EncodeError::new(ErrorKind::UnsupportedParam, None))
		})
		.expect_err("the driver rejected the change");
		assert_eq!(error.kind(), ErrorKind::UnsupportedParam);
		assert_eq!(rates(&config), (1_000_000, 100_000, 100_000));

		// Scaled from the last accepted rate, not the rejected one.
		retune(&init, &mut config, 2_000_000, |params| {
			assert_eq!(submitted(params), (2_000_000, 200_000, 200_000));
			Ok(())
		})
		.unwrap();
		assert_eq!(rates(&config), (2_000_000, 200_000, 200_000));
	}

	#[test]
	fn invalid_rate_change_is_refused_before_the_driver() {
		let init = NV_ENC_INITIALIZE_PARAMS::default();
		let mut config = rate_config(1, u32::MAX);
		for bitrate in [0, 2] {
			let error = retune(&init, &mut config, bitrate, |_| panic!("submitted an invalid rate"))
				.expect_err("the rate is invalid");
			assert_eq!(error.kind(), ErrorKind::InvalidParam);
			assert_eq!(rates(&config), (1, u32::MAX, u32::MAX));
		}
	}

	#[test]
	fn failed_wait_abandons_without_waiting_again() {
		let events = Events::default();
		assert!(pending(&events, true).finish().is_err());
		assert_eq!(*events.lock().unwrap(), ["wait", "abandon"]);

		let events = Events::default();
		drop(pending(&events, true));
		assert_eq!(*events.lock().unwrap(), ["wait", "abandon"]);
	}

	#[test]
	fn session_identity_rejects_cross_session_resources() {
		let first = Arc::new(());
		let second = Arc::new(());
		assert!(same_session(&first, &first, &first));
		assert!(!same_session(&first, &second, &first));
	}

	/// Whether an NVENC session can run here. Hardware tests return early
	/// without one, so they pass on GPU-less CI.
	fn driver_available() -> bool {
		// cudarc panics while loading a missing libcuda, so probe for it first.
		// SAFETY: the library is opened only to test presence, never called.
		let cuda = ["libcuda.so.1", "libcuda.so"]
			.iter()
			.any(|name| unsafe { libloading::Library::new(*name) }.is_ok());
		cuda && Encoder::load().is_ok()
	}

	/// Lookahead holds frames back, and the driver refuses to lock a held
	/// frame's output. P7 with high-quality tuning turns lookahead on. The failed
	/// submissions must not keep the session open: one still open at exit
	/// deadlocks the driver's exit handler, so the process never exits.
	#[test]
	fn failed_submission_releases_the_session() {
		if !driver_available() {
			return;
		}
		// The libraries can load when no device is assigned. That is the same
		// as a missing driver: only a session that can start proves the fix.
		let Ok(cuda) = CudaContext::new(0) else {
			return;
		};
		let Ok(encoder) = Encoder::initialize_with_cuda(cuda) else {
			return;
		};
		let (codec, preset, tuning) = (
			NV_ENC_CODEC_H264_GUID,
			NV_ENC_PRESET_P7_GUID,
			NV_ENC_TUNING_INFO_HIGH_QUALITY,
		);
		let mut config = encoder.get_preset_config(codec, preset, tuning).unwrap().presetCfg;
		assert_eq!(
			config.rcParams.enableLookahead(),
			1,
			"the preset no longer holds frames"
		);
		// No B-frames, so lookahead alone holds the frames.
		config.frameIntervalP = 1;

		let mut init = EncoderInitParams::new(codec, 320, 240);
		init.preset_guid(preset)
			.tuning_info(tuning)
			.enable_picture_type_decision();
		// SAFETY: the preset config holds no borrowed extension pointers.
		unsafe { init.encode_config(config) };
		let session = encoder.start_session(NV_ENC_BUFFER_FORMAT_NV12, init).unwrap();
		let encoder = Arc::downgrade(&session.encoder);

		let submit = || {
			let input = session.create_input_buffer().unwrap();
			let output = session.create_output_bitstream().unwrap();
			session
				.encode_picture(input, output, EncodePictureParams::default())
				.unwrap()
		};
		// One submission fails to finish and one is dropped unfinished.
		assert!(submit().finish().is_err(), "the driver locked a held frame's output");
		drop(submit());

		drop(session);
		assert!(encoder.upgrade().is_none(), "failed submissions kept the session open");
	}
}

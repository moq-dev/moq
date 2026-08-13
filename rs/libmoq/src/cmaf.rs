//! Single-rendition CMAF muxing for application-selected encoded frames.

use std::ffi::c_char;
use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;

use crate::{Error, Id, NonZeroSlab, State, ffi};

const CONTAINER_LEGACY: u32 = 0;
const CONTAINER_CMAF: u32 = 1;
const CONTAINER_LOC: u32 = 2;

/// Video rendition metadata used to create a CMAF muxer.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_cmaf_video_config {
	/// Codec string, such as `avc3.42c01f`, not NULL terminated.
	pub codec: *const c_char,
	/// Length of `codec` in bytes.
	pub codec_len: usize,
	/// Optional codec description bytes.
	pub description: *const u8,
	/// Length of `description` in bytes.
	pub description_len: usize,
	/// Encoded width in pixels, or zero when unknown.
	pub coded_width: u32,
	/// Encoded height in pixels, or zero when unknown.
	pub coded_height: u32,
	/// Frame rate, or zero when unknown.
	pub framerate: f64,
	/// Input container: 0 for Legacy, 1 for CMAF, or 2 for LOC.
	pub container: u32,
	/// Existing CMAF initialization bytes when `container` selects CMAF.
	pub container_init: *const u8,
	/// Length of `container_init` in bytes.
	pub container_init_len: usize,
	/// Timestamp subtracted from every output sample, in microseconds.
	pub origin_us: u64,
}

/// Audio rendition metadata used to create a CMAF muxer.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_cmaf_audio_config {
	/// Codec string, such as `opus`, not NULL terminated.
	pub codec: *const c_char,
	/// Length of `codec` in bytes.
	pub codec_len: usize,
	/// Optional codec description bytes.
	pub description: *const u8,
	/// Length of `description` in bytes.
	pub description_len: usize,
	/// Audio sample rate in Hz.
	pub sample_rate: u32,
	/// Number of audio channels.
	pub channel_count: u32,
	/// Input container: 0 for Legacy, 1 for CMAF, or 2 for LOC.
	pub container: u32,
	/// Existing CMAF initialization bytes when `container` selects CMAF.
	pub container_init: *const u8,
	/// Length of `container_init` in bytes.
	pub container_init_len: usize,
	/// Timestamp subtracted from every output sample, in microseconds.
	pub origin_us: u64,
}

/// One encoded sample supplied to a CMAF muxer.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_cmaf_frame {
	/// Encoded sample bytes borrowed for the duration of the mux call.
	pub payload: *const u8,
	/// Length of `payload` in bytes.
	pub payload_size: usize,
	/// Presentation timestamp in microseconds.
	pub timestamp_us: u64,
	/// Whether the sample can be decoded without an earlier sample.
	pub keyframe: bool,
	/// Exact sample duration in microseconds when `has_duration` is true.
	pub duration_us: u64,
	/// Whether `duration_us` is present.
	pub has_duration: bool,
}

/// Borrowed CMAF initialization and media bytes produced by one mux call.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_cmaf_output {
	/// Current initialization bytes, or NULL while inline codec metadata is unresolved.
	pub initialization: *const u8,
	/// Length of `initialization` in bytes.
	pub initialization_size: usize,
	/// Media fragment bytes, or NULL when the batch produced no usable samples.
	pub fragment: *const u8,
	/// Length of `fragment` in bytes.
	pub fragment_size: usize,
}

#[derive(Default)]
pub(crate) struct Cmaf {
	muxers: NonZeroSlab<Muxer>,
}

struct Muxer {
	inner: moq_mux::container::fmp4::Muxer,
	origin: Duration,
	initialization: Option<Bytes>,
	fragment: Option<Bytes>,
}

impl Cmaf {
	fn insert(&mut self, inner: moq_mux::container::fmp4::Muxer, origin_us: u64) -> Result<Id, Error> {
		self.muxers.insert(Muxer {
			inner,
			origin: Duration::from_micros(origin_us),
			initialization: None,
			fragment: None,
		})
	}

	fn initialization(&mut self, id: Id, dst: &mut moq_cmaf_output) -> Result<(), Error> {
		let muxer = self.muxers.get_mut(id).ok_or(Error::MediaNotFound)?;
		muxer.initialization = muxer.inner.init()?;
		muxer.fragment = None;
		write_output(dst, muxer);
		Ok(())
	}

	fn mux(
		&mut self,
		id: Id,
		sequence: u32,
		frames: Vec<moq_mux::container::Frame>,
		dst: &mut moq_cmaf_output,
	) -> Result<(), Error> {
		let muxer = self.muxers.get_mut(id).ok_or(Error::MediaNotFound)?;
		let output = muxer.inner.mux(sequence, muxer.origin, frames)?;
		muxer.initialization = output.initialization;
		muxer.fragment = output.fragment;
		write_output(dst, muxer);
		Ok(())
	}

	fn close(&mut self, id: Id) -> Result<(), Error> {
		self.muxers.remove(id).ok_or(Error::MediaNotFound)?;
		Ok(())
	}
}

fn write_output(dst: &mut moq_cmaf_output, muxer: &Muxer) {
	let (initialization, initialization_size) = borrowed_bytes(muxer.initialization.as_ref());
	let (fragment, fragment_size) = borrowed_bytes(muxer.fragment.as_ref());
	*dst = moq_cmaf_output {
		initialization,
		initialization_size,
		fragment,
		fragment_size,
	};
}

fn borrowed_bytes(data: Option<&Bytes>) -> (*const u8, usize) {
	match data {
		Some(data) => (data.as_ptr(), data.len()),
		None => (std::ptr::null(), 0),
	}
}

unsafe fn description(data: *const u8, len: usize) -> Result<Option<Bytes>, Error> {
	if data.is_null() {
		return if len == 0 { Ok(None) } else { Err(Error::InvalidPointer) };
	}
	Ok(Some(Bytes::copy_from_slice(unsafe { ffi::parse_slice(data, len)? })))
}

unsafe fn container(kind: u32, init: *const u8, init_len: usize) -> Result<hang::catalog::Container, Error> {
	Ok(match kind {
		CONTAINER_LEGACY => hang::catalog::Container::Legacy,
		CONTAINER_CMAF => {
			let init = unsafe { ffi::parse_slice(init, init_len)? };
			hang::catalog::Container::Cmaf {
				init: Bytes::copy_from_slice(init),
			}
		}
		CONTAINER_LOC => hang::catalog::Container::Loc,
		_ => return Err(Error::InvalidCode),
	})
}

/// Create a CMAF muxer for one video rendition.
///
/// Returns a non-zero muxer handle on success or a negative error code.
///
/// # Safety
/// `config` and every non-NULL buffer it references must remain valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_cmaf_video(config: *const moq_cmaf_video_config) -> i32 {
	ffi::enter(move || {
		let raw = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let codec = unsafe { ffi::parse_str(raw.codec, raw.codec_len)? };
		let codec = hang::catalog::VideoCodec::from_str(codec).map_err(Error::Hang)?;
		let mut config = hang::catalog::VideoConfig::new(codec);
		config.description = unsafe { description(raw.description, raw.description_len)? };
		if raw.coded_width != 0 && raw.coded_height != 0 {
			config.coded_width = Some(raw.coded_width);
			config.coded_height = Some(raw.coded_height);
		}
		config.framerate = (raw.framerate.is_finite() && raw.framerate > 0.0).then_some(raw.framerate);
		config.container = unsafe { container(raw.container, raw.container_init, raw.container_init_len)? };
		let muxer = moq_mux::container::fmp4::Muxer::video(&config)?;
		State::lock().cmaf.insert(muxer, raw.origin_us)
	})
}

/// Create a CMAF muxer for one audio rendition.
///
/// Returns a non-zero muxer handle on success or a negative error code.
///
/// # Safety
/// `config` and every non-NULL buffer it references must remain valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_cmaf_audio(config: *const moq_cmaf_audio_config) -> i32 {
	ffi::enter(move || {
		let raw = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let codec = unsafe { ffi::parse_str(raw.codec, raw.codec_len)? };
		let codec = hang::catalog::AudioCodec::from_str(codec).map_err(Error::Hang)?;
		let mut config = hang::catalog::AudioConfig::new(codec, raw.sample_rate, raw.channel_count);
		config.description = unsafe { description(raw.description, raw.description_len)? };
		config.container = unsafe { container(raw.container, raw.container_init, raw.container_init_len)? };
		let muxer = moq_mux::container::fmp4::Muxer::audio(&config)?;
		State::lock().cmaf.insert(muxer, raw.origin_us)
	})
}

/// Read the muxer's current initialization segment.
///
/// Returned buffers remain valid until the next call using this muxer or until it is closed.
///
/// # Safety
/// `dst` must point to writable `moq_cmaf_output` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_cmaf_init(muxer: u32, dst: *mut moq_cmaf_output) -> i32 {
	ffi::enter(move || {
		let muxer = ffi::parse_id(muxer)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().cmaf.initialization(muxer, dst)
	})
}

/// Normalize and encode one batch of frames on the muxer's presentation timeline.
///
/// A batch must not cross an inline codec configuration boundary. Split and retry at the
/// input frame index reported by `moq_error` when a rendition is reconfigured.
///
/// Returned buffers remain valid until the next call using this muxer or until it is closed.
///
/// # Safety
/// `frames` must point to `frame_count` valid frames, each payload must be readable for its
/// declared size, and `dst` must point to writable `moq_cmaf_output` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_cmaf_mux(
	muxer: u32,
	sequence: u32,
	frames: *const moq_cmaf_frame,
	frame_count: usize,
	dst: *mut moq_cmaf_output,
) -> i32 {
	ffi::enter(move || {
		let muxer = ffi::parse_id(muxer)?;
		let frames = if frame_count == 0 {
			&[][..]
		} else {
			if frames.is_null() {
				return Err(Error::InvalidPointer);
			}
			unsafe { std::slice::from_raw_parts(frames, frame_count) }
		};
		let frames = frames
			.iter()
			.map(|frame| {
				let payload = unsafe { ffi::parse_slice(frame.payload, frame.payload_size)? };
				Ok(moq_mux::container::Frame {
					payload: Bytes::copy_from_slice(payload),
					timestamp: moq_net::Timestamp::from_micros(frame.timestamp_us)?,
					keyframe: frame.keyframe,
					duration: frame
						.has_duration
						.then(|| moq_net::Timestamp::from_micros(frame.duration_us))
						.transpose()?,
				})
			})
			.collect::<Result<Vec<_>, Error>>()?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().cmaf.mux(muxer, sequence, frames, dst)
	})
}

/// Release a CMAF muxer and invalidate its borrowed output buffers.
#[unsafe(no_mangle)]
pub extern "C" fn moq_cmaf_close(muxer: u32) -> i32 {
	ffi::enter(move || {
		let muxer = ffi::parse_id(muxer)?;
		State::lock().cmaf.close(muxer)
	})
}

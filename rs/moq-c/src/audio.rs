//! Raw-audio import/export via [`moq_audio`].
//!
//! Sibling to `moq_publish_media_*` / `moq_consume_audio`
//! (those handle already-encoded frames). These functions accept and
//! return raw PCM, with Opus encode/decode happening inside the FFI
//! boundary.
//!
//! Format / sample rate / channel count are fixed at producer or
//! consumer construction via [`moq_audio_encoder_input`] /
//! [`moq_audio_encoder_output`] / [`moq_audio_decoder_output`], so
//! each [`moq_audio_frame`] carries only payload bytes and a
//! timestamp.

use std::ffi::{c_char, c_void};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::oneshot;

use crate::decode::Read;
use crate::ffi::OnStatus;
use crate::{moq_decode_gap, moq_decode_read_kind, moq_decode_start};

use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, Shared, State, ffi};

// ---- C-visible types ----

/// Raw PCM sample layout, mirroring WebCodecs `AudioData.format`.
///
/// The enum is exposed in the C header for readability, but ABI
/// fields/parameters that carry it are typed `u32`. A C caller
/// passing an unknown discriminant gets `Error::InvalidCode` instead
/// of UB.
///
/// <https://developer.mozilla.org/en-US/docs/Web/API/AudioData/format>
#[repr(C)]
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
#[derive(Clone, Copy, Debug)]
pub enum moq_audio_sample_format {
	/// Unsigned 8-bit PCM samples, interleaved by channel.
	MOQ_AUDIO_SAMPLE_FORMAT_U8 = 0,
	/// Signed 16-bit PCM samples, interleaved by channel.
	MOQ_AUDIO_SAMPLE_FORMAT_S16 = 1,
	/// Signed 32-bit PCM samples, interleaved by channel.
	MOQ_AUDIO_SAMPLE_FORMAT_S32 = 2,
	/// 32-bit floating-point PCM samples, interleaved by channel.
	MOQ_AUDIO_SAMPLE_FORMAT_F32 = 3,
	/// Unsigned 8-bit PCM samples, one plane per channel.
	MOQ_AUDIO_SAMPLE_FORMAT_U8_PLANAR = 4,
	/// Signed 16-bit PCM samples, one plane per channel.
	MOQ_AUDIO_SAMPLE_FORMAT_S16_PLANAR = 5,
	/// Signed 32-bit PCM samples, one plane per channel.
	MOQ_AUDIO_SAMPLE_FORMAT_S32_PLANAR = 6,
	/// 32-bit floating-point PCM samples, one plane per channel.
	MOQ_AUDIO_SAMPLE_FORMAT_F32_PLANAR = 7,
}

fn audio_format_from_u32(value: u32) -> Result<moq_audio::Format, Error> {
	use moq_audio::Format;
	Ok(match value {
		v if v == moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_U8 as u32 => Format::U8,
		v if v == moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_S16 as u32 => Format::S16,
		v if v == moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_S32 as u32 => Format::S32,
		v if v == moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_F32 as u32 => Format::F32,
		v if v == moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_U8_PLANAR as u32 => Format::U8Planar,
		v if v == moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_S16_PLANAR as u32 => Format::S16Planar,
		v if v == moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_S32_PLANAR as u32 => Format::S32Planar,
		v if v == moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_F32_PLANAR as u32 => Format::F32Planar,
		_ => return Err(Error::InvalidCode),
	})
}

/// PCM layout the caller hands to [`moq_encode_audio_frame`].
#[repr(C)]
pub struct moq_audio_encoder_input {
	/// `moq_audio_sample_format` discriminant.
	pub format: u32,
	/// The number of PCM samples per second.
	pub sample_rate: u32,
	/// The channel count, which also names the speaker layout by the
	/// WAVE convention: 1 mono, 2 stereo, 3 2.1, 4 quad, 5 5.0, 6 5.1, 7 6.1,
	/// 8 7.1, in front left, front right, center, LFE, back, side order.
	pub channels: u32,
}

/// Codec-side configuration. `sample_rate` / `channels` = 0 means
/// "match the input (snapping the rate up to a libopus-supported
/// value if necessary)".
#[repr(C)]
pub struct moq_audio_encoder_output {
	/// Codec id, UTF-8: "opus", "pcm", or "aac". AAC encodes through the
	/// platform's encoder, so a host without one refuses it.
	pub codec: *const c_char,
	/// The length in bytes of `codec`.
	pub codec_len: usize,
	/// 0 = derive from input.
	pub sample_rate: u32,
	/// 0 = derive from input.
	pub channels: u32,
	/// 0 = libopus default.
	pub bitrate: u32,
	/// Encoded frame duration in microseconds. Opus accepts exactly
	/// 2500/5000/10000/20000/40000/60000 us. 0 = the codec's default: 20 ms for
	/// Opus, which matches the JS publish path, and 1024 samples for AAC.
	pub frame_duration_us: u32,
}

/// PCM layout the caller wants out of [`moq_decode_audio`].
#[repr(C)]
pub struct moq_audio_decoder_output {
	/// The moq_audio_sample_format discriminant.
	pub format: u32,
	/// 0 = deliver at the codec's native sample rate.
	pub sample_rate: u32,
	/// 0 = deliver at the codec's native channel count. A count names its
	/// layout as `moq_audio_encoder_input.channels` describes, and the decoder
	/// remixes to it.
	pub channels: u32,
	/// Upper bound on buffering before skipping a stalled group, in
	/// microseconds. Same congestion-control knob as
	/// `moq_consume_audio`'s `max_delay_us`. 0 = skip
	/// aggressively (the moq-mux default); set to your playout
	/// buffer (tens to a few hundred ms) for a softer skip. This bounds
	/// group staleness rather than setting a minimum playout delay.
	pub max_delay_us: u64,
	/// `moq_decode_start` discriminant: where to start on a track that already
	/// holds groups. 0 = the oldest, decoding everything cached; a live player
	/// sets `MOQ_DECODE_START_LATEST`.
	pub start: u32,
}

/// One audio frame: payload bytes plus a presentation timestamp.
///
/// `data` is owned by the consume slab (see
/// [`moq_decode_audio_read_free`]) or borrowed by the publish call
/// (the publisher copies before returning).
#[repr(C)]
pub struct moq_audio_frame {
	/// The presentation timestamp in microseconds.
	pub timestamp_us: u64,
	/// The raw frame bytes in the configured layout.
	pub data: *const u8,
	/// The length in bytes of `data`.
	pub data_size: usize,
}

/// One read from [`moq_decode_audio`]: decoded PCM, or a break in it. `kind` says which
/// field is filled; the others are zeroed.
#[repr(C)]
pub struct moq_audio_read {
	/// Which kind of read this is.
	pub kind: moq_decode_read_kind,
	/// The decoded PCM, for [moq_decode_read_kind::MOQ_DECODE_READ_FRAME].
	pub frame: moq_audio_frame,
	/// The lost span, for [moq_decode_read_kind::MOQ_DECODE_READ_GAP]. What follows does
	/// not continue what came before.
	pub gap: moq_decode_gap,
	/// Where the stopped encoder's media stops in microseconds, for
	/// [moq_decode_read_kind::MOQ_DECODE_READ_END]. Frames that follow and start before it
	/// are the stopped encoder's remaining media; the first at or past it comes from a fresh
	/// encoder.
	pub end_us: u64,
}

// ---- State extensions (used internally by lib.rs) ----

/// An audio producer, shared so the Opus encode in `write` runs with the global
/// lock released. See [`Shared`].
pub(crate) struct AudioEncoder {
	producer: moq_audio::encode::Producer,
	reservation: Option<Arc<moq_net::bandwidth::Reservation>>,
	children: crate::publish::Children,
}

type AudioProducer = Shared<AudioEncoder>;

impl crate::publish::Abort for AudioProducer {
	fn abort(self, err: moq_net::Error) {
		if let Some(encoder) = self.take() {
			encoder.producer.abort(err);
		}
	}
}

#[derive(Default)]
/// The audio encoder, decoder, and decoded frame handle tables.
pub struct Audio {
	producers: NonZeroSlab<AudioProducer>,
	consumer_tasks: NonZeroSlab<Task>,
	reads: NonZeroSlab<Read<moq_audio::Frame>>,
}

impl Audio {
	/// Encode PCM frames into an audio track and return its handle.
	pub fn publish(
		&mut self,
		target: crate::publish::Target<'_>,
		input: moq_audio::encode::Input,
		options: moq_audio::encode::Options,
		reserve: bool,
	) -> Result<Id, Error> {
		let producer = moq_audio::encode::Producer::new(target.producer, target.catalog.clone(), input, &options)?;
		let reservation = reserve.then(|| Arc::new(options.bandwidth.reserve(&producer.demand(), producer.bitrate())));
		let encoder = Shared::new(AudioEncoder {
			producer,
			reservation,
			children: target.children.clone(),
		});
		target
			.children
			.insert(&mut self.producers, encoder, crate::publish::Child::Audio)
	}

	pub(crate) fn reservation(&self, id: Id) -> Result<Option<Arc<moq_net::bandwidth::Reservation>>, Error> {
		Ok(self
			.producer(id)?
			.lock()
			.as_ref()
			.ok_or(Error::MediaNotFound)?
			.reservation
			.clone())
	}

	/// A watch-only handle to the encoded track's subscriber demand.
	pub(crate) fn demand(&self, id: Id) -> Result<moq_net::track::Demand, Error> {
		Ok(self
			.producer(id)?
			.lock()
			.as_ref()
			.ok_or(Error::MediaNotFound)?
			.producer
			.demand())
	}

	/// Resolve a producer handle, so the caller can encode with the global lock
	/// released.
	///
	/// Bind the result before locking it: a temporary [`State`] guard lives to the
	/// end of the statement that created it, so resolving and locking in one
	/// expression would put the encode back under the global lock.
	pub(crate) fn producer(&self, id: Id) -> Result<AudioProducer, Error> {
		self.producers.get(id).cloned().ok_or(Error::MediaNotFound)
	}

	/// Resolve a producer and drop its id, so nothing can be published to it after.
	pub(crate) fn remove(&mut self, id: Id) -> Result<AudioProducer, Error> {
		self.producers.remove(id).ok_or(Error::MediaNotFound)
	}

	/// Decode a rendition and deliver read handles to the callback.
	pub fn consume(
		&mut self,
		rendition: crate::consume::Rendition<hang::catalog::AudioConfig>,
		config: moq_audio::decode::Options,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let crate::consume::Rendition {
			broadcast,
			config: catalog,
			name,
			driver,
		} = rendition;

		// `decode::Consumer::new` subscribes (blocking on SUBSCRIBE_OK), so run it
		// inside the task to keep this entrypoint non-blocking.
		self.consumer_tasks.spawn(
			on_frame,
			|state| &mut state.audio.consumer_tasks,
			|close| async move {
				driver
					.watch(async move {
						let consumer = moq_audio::decode::Consumer::new(&broadcast, &catalog, name, config).await?;
						Self::run(on_frame, consumer, close).await
					})
					.await
			},
		)
	}

	async fn run(
		callback: OnStatus,
		mut consumer: moq_audio::decode::Consumer,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready read.
			let read = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				read = consumer.read() => match read {
					Ok(Some(moq_audio::decode::Read::Frame(frame))) => Read::Frame(frame),
					Ok(Some(moq_audio::decode::Read::Gap { timestamp, duration })) => Read::gap(timestamp, duration)?,
					Ok(Some(moq_audio::decode::Read::End { end })) => Read::end(end)?,
					Ok(None) => return Ok(()),
					// One packet the codec rejected is that packet's problem: the
					// decoder stays usable, so drop it and keep the subscription rather
					// than ending the caller's stream over a single bad frame.
					Err(moq_audio::Error::Decode(err)) => {
						tracing::warn!(%err, "dropping an audio frame");
						continue;
					}
					Err(err) => return Err(err.into()),
				},
			};

			// Hold the lock only to buffer the read; release it before the callback.
			let read_id = State::lock().audio.reads.insert(read)?;
			callback.call(Ok(read_id));
		}
	}

	/// Stop decoding and release the consumer handle.
	pub fn consume_close(&mut self, id: Id) -> Result<(), Error> {
		self.consumer_tasks.close(id, Error::TrackNotFound)
	}

	/// Borrow a delivered read, and a frame's samples, until its handle is freed.
	pub fn read_info(&self, id: Id, dst: &mut moq_audio_read) -> Result<(), Error> {
		let read = self.reads.get(id).ok_or(Error::FrameNotFound)?;
		let (kind, gap, end_us) = read.info();
		let frame = match read {
			Read::Frame(frame) => moq_audio_frame {
				// The C ABI carries plain microseconds, so flatten the scaled
				// `Timestamp` here at the boundary. Saturating rather than erroring:
				// this is a getter on a frame we already decoded, and a u64 overflow
				// needs a timestamp ~580,000 years out.
				timestamp_us: u64::try_from(frame.timestamp.as_micros()).unwrap_or(u64::MAX),
				data: frame.data.as_ptr(),
				data_size: frame.data.len(),
			},
			_ => moq_audio_frame {
				timestamp_us: 0,
				data: std::ptr::null(),
				data_size: 0,
			},
		};
		*dst = moq_audio_read {
			kind,
			frame,
			gap,
			end_us,
		};
		Ok(())
	}

	/// Release a delivered read handle.
	pub fn read_free(&mut self, id: Id) -> Result<(), Error> {
		self.reads.remove(id).ok_or(Error::FrameNotFound)?;
		Ok(())
	}
}

// ---- C entry points ----

/// Open an audio track on a broadcast.
///
/// The encoder configuration is fixed at construction; subsequent
/// frame writes pass only payload + timestamp via
/// [`moq_encode_audio_frame`].
///
/// Returns a non-zero handle on success or a negative error code.
///
/// # Safety
/// - `name` must point to `name_len` bytes of UTF-8.
/// - `input` / `output` must point to fully populated structs.
/// - `output->codec` must point to `output->codec_len` bytes of UTF-8.
/// - `allocator` is a handle from [`crate::moq_session_allocator`], or 0 to leave the
///   configured bitrate unclaimed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_audio(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	input: *const moq_audio_encoder_input,
	output: *const moq_audio_encoder_output,
	allocator: u32,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? }.to_string();
		let raw_input = unsafe { input.as_ref() }.ok_or(Error::InvalidPointer)?;
		let raw_output = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;
		let codec_str = unsafe { ffi::parse_str(raw_output.codec, raw_output.codec_len)? };

		let layout = moq_audio::Layout::from_channels(raw_input.channels)?;
		let mut encoder_input = moq_audio::encode::Input::new(raw_input.sample_rate, layout);
		encoder_input.format = audio_format_from_u32(raw_input.format)?;

		// The C ABI takes an explicit track name and spells "unset" as 0, so map
		// both onto the Rust options here rather than leaking either convention.
		let mut options = moq_audio::encode::Options::default();
		options.track = Some(name);
		let codec = codec_str
			.parse()
			.map_err(|_| Error::UnknownFormat(codec_str.to_string()))?;
		options.config = moq_audio::encode::Config::from_input(codec, &encoder_input);
		if let Some(sample_rate) = zeroable(raw_output.sample_rate) {
			options.config.sample_rate = sample_rate;
		}
		if let Some(channels) = zeroable(raw_output.channels) {
			options.config.layout = moq_audio::Layout::from_channels(channels)?;
		}
		options.config.bitrate = zeroable(raw_output.bitrate).map(|bps| moq_net::bandwidth::Rate::from_bps(bps.into()));
		if let Some(micros) = zeroable(raw_output.frame_duration_us) {
			options.config.frame_duration = Duration::from_micros(micros.into());
		}

		let allocator = ffi::parse_id_optional(allocator)?;
		let mut state = State::lock();
		if let Some(id) = allocator {
			options.bandwidth = state.bandwidth.allocator(id)?;
		}
		let State { publish, audio, .. } = &mut *state;
		audio.publish(publish.target(broadcast)?, encoder_input, options, allocator.is_some())
	})
}

/// This encoder's bandwidth reservation, or 0 if it was published without one.
///
/// Closing the returned handle does not release the encoder's claim; that lasts
/// until [moq_encode_audio_finish].
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_audio_reservation(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let mut state = State::lock();
		match state.audio.reservation(producer)? {
			Some(reservation) => Ok(i32::from(state.bandwidth.hold(reservation)?)),
			None => Ok(0),
		}
	})
}

/// Watch whether the encoded audio track has subscribers, so the microphone and
/// encoder run only while someone listens. See [`crate::moq_publish_media_demand`]
/// for the callback contract.
///
/// Returns a non-zero watcher handle on success, or a negative code on failure.
///
/// # Safety
/// - `on_demand` must be non-NULL.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_demand` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_audio_demand(
	producer: u32,
	on_demand: crate::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let on_demand = unsafe { OnStatus::new(user_data, on_demand)? };
		let mut state = State::lock();
		let demand = state.audio.demand(producer)?;
		state.publish.demand(demand, on_demand)
	})
}

/// The C ABI spells an unset `u32` knob as 0, which no field here accepts as a
/// real value.
fn zeroable(value: u32) -> Option<u32> {
	(value != 0).then_some(value)
}

/// Push one audio frame.
///
/// `frame->data` is borrowed for the duration of the call; the
/// producer copies before returning.
///
/// # Safety
/// - `frame` must point to a valid [`moq_audio_frame`].
/// - `frame->data` must point to `frame->data_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_audio_frame(producer: u32, frame: *const moq_audio_frame) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let frame = unsafe { frame.as_ref() }.ok_or(Error::InvalidPointer)?;
		let data = unsafe { ffi::parse_slice(frame.data, frame.data_size)? };

		// The C ABI carries plain microseconds; scale them at the boundary.
		let timestamp = moq_net::Timestamp::from_micros(frame.timestamp_us).map_err(moq_audio::Error::from)?;
		let owned = moq_audio::Frame::new(Bytes::copy_from_slice(data), timestamp);

		let producer = State::lock().audio.producer(producer)?;
		producer
			.lock()
			.as_mut()
			.ok_or(Error::MediaNotFound)?
			.producer
			.write(&owned)?;
		Ok(())
	})
}

/// Flush any pending samples and finalize an audio producer.
///
/// The broadcast keeps the finished track, catalog rendition included, until it closes, so a
/// late subscriber can still drain it.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_audio_finish(producer: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(producer)?;
		// The id stays until the flush ends, so a racing moq_publish_close waits it out
		// instead of missing a track that still publishes. A call queued behind the flush
		// finds the encoder gone.
		let shared = State::lock().audio.producer(id)?;
		let finished: Result<(), Error> = (|| {
			let mut guard = shared.lock();
			let AudioEncoder {
				mut producer, children, ..
			} = guard.take().ok_or(Error::MediaNotFound)?;
			if let Err(err) = producer.finish() {
				// Readers see the failure rather than `Dropped`, as a failed video drain does.
				children.forget(crate::publish::Child::Audio(id));
				producer.abort(moq_net::Error::Transport(err.to_string()));
				return Err(err.into());
			}
			// The broadcast keeps the finished track, so its finish still holds it.
			children.keep(crate::publish::Child::Audio(id), producer);
			Ok(())
		})();
		// A racing close may have released the id already.
		let _ = State::lock().audio.remove(id);
		finished
	})
}

/// Subscribe to an audio track and decode it into PCM.
///
/// The catalog `index` identifies which audio rendition to subscribe
/// to, matching the existing `moq_consume_audio` selection
/// model. TODO: a future API will pick the right rendition
/// automatically (ABR).
///
/// Returns a non-zero handle on success or a negative error code.
///
/// `on_read` is called with a positive read id per decoded frame, gap, or encoder
/// end (see [`moq_decode_audio_read`]), then exactly once more with a terminal
/// code: `0` (closed cleanly) or a negative error. After the terminal (`<= 0`)
/// callback, `on_read` is never called again and `user_data` is never touched
/// again, so release `user_data` there. The terminal callback fires even after
/// [`moq_decode_audio_cancel`].
///
/// `output->start` picks where it starts on a track that already holds groups.
///
/// A packet the codec cannot decode is logged and skipped rather than ending
/// the subscription, so a single bad frame costs that frame and not the stream.
///
/// # Safety
/// - `output` must point to a valid [`moq_audio_decoder_output`].
/// - `user_data` must stay valid until the terminal (`<= 0`) `on_read` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_decode_audio(
	catalog: u32,
	index: u32,
	output: *const moq_audio_decoder_output,
	on_read: crate::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let raw = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;

		let mut config = moq_audio::decode::Options::default();
		config.start = match moq_decode_start::from_u32(raw.start)? {
			moq_decode_start::MOQ_DECODE_START_OLDEST => moq_audio::decode::Start::Oldest,
			moq_decode_start::MOQ_DECODE_START_LATEST => moq_audio::decode::Start::Latest,
		};
		config.output.format = audio_format_from_u32(raw.format)?;
		config.output.sample_rate = zeroable(raw.sample_rate);
		config.output.layout = zeroable(raw.channels)
			.map(moq_audio::Layout::from_channels)
			.transpose()?;
		config.max_delay = Duration::from_micros(raw.max_delay_us);

		let on_read = unsafe { OnStatus::new(user_data, on_read)? };

		let mut state = State::lock();
		let rendition = state.consume.rendition(catalog, index as usize)?;

		let State { audio, .. } = &mut *state;
		audio.consume(rendition, config, on_read)
	})
}

/// Stop an audio (raw PCM) consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the on-read callback still fires once more with a
/// terminal `0` (or a negative error), which is where `user_data` should be
/// released. Read ids already delivered to the callback are likewise not freed;
/// release each with [`moq_decode_audio_read_free`].
#[unsafe(no_mangle)]
pub extern "C" fn moq_decode_audio_cancel(consumer: u32) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		State::lock().audio.consume_close(consumer)
	})
}

/// Copy a delivered read into `dst`: its kind, and the frame, gap, or end it carries.
///
/// The written `dst->frame.data` pointer remains valid until the same `id`
/// is released with [`moq_decode_audio_read_free`].
///
/// # Safety
/// - `dst` must point to a writable [`moq_audio_read`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_decode_audio_read(id: u32, dst: *mut moq_audio_read) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().audio.read_info(id, dst)
	})
}

/// Free a read previously delivered through the decode callback.
/// Required for every delivered read id; closing the parent consumer
/// is not enough.
#[unsafe(no_mangle)]
pub extern "C" fn moq_decode_audio_read_free(id: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		State::lock().audio.read_free(id)
	})
}

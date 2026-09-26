//! Audio decoder front end.
//!
//! Mirror of [`encode::Encoder`](crate::encode::Encoder): dispatches over the
//! catalog codec and produces interleaved `f32` PCM.

use unsafe_libopus::{
	OPUS_OK, OPUS_RESET_STATE, OPUS_SET_GAIN_REQUEST, OpusDecoder, opus_decode_float, opus_decoder_create,
	opus_decoder_ctl_impl, opus_decoder_destroy, varargs,
};

#[cfg(feature = "aac")]
use symphonia_core::codecs::audio::AudioDecoder;

use super::Decoded;
#[cfg(feature = "aac")]
use crate::aac;
use crate::opus;
use crate::pcm;
use crate::{Activity, Error, Layout};

/// Opus packets cap at 120 ms (RFC 6716 §2.1.4).
const MAX_FRAME_MS: usize = 120;

/// Decoder backend selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Pick the available backend automatically.
	#[default]
	Auto,
	/// Require the built-in software backend.
	Software,
	/// Require a backend by its stable lowercase name.
	Named(String),
}

/// Low-level decoder configuration.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Backend selection policy.
	pub kind: Kind,
}

impl Config {
	/// Select the available backend automatically.
	pub fn new() -> Self {
		Self::default()
	}
}

/// Decodes codec packets into interleaved `f32` PCM.
///
/// The bring-your-own-payload layer under [`Consumer`](super::Consumer): use it
/// when the packets don't come from a plain track subscription.
pub struct Decoder {
	backend: Backend,
	sample_rate: u32,
	layout: Layout,
	delay: usize,
}

enum Backend {
	Opus(Opus),
	Pcm {
		bytes_per_frame: usize,
	},
	#[cfg(feature = "aac")]
	Aac(Box<Aac>),
}

struct Opus {
	inner: *mut OpusDecoder,
	pre_skip_remaining: usize,
	max_frame_size: usize,
	in_dtx: bool,
}

// SAFETY: see Encoder.
unsafe impl Send for Opus {}

/// Boxed in [`Backend`]: the symphonia decoder carries its own filterbank state,
/// which is far larger than the other backends' handles.
#[cfg(feature = "aac")]
struct Aac {
	inner: symphonia_codec_aac::AacDecoder,
}

impl Decoder {
	/// Build a decoder from a catalog [`AudioConfig`](hang::catalog::AudioConfig).
	///
	/// Opus decodes at 48 kHz with the pre-skip and gain its OpusHead
	/// `description` declares, refusing a malformed head or a channel mapping
	/// other than mono/stereo; without a description it takes the catalog's
	/// channel count and applies neither. PCM uses the catalog fields directly
	/// and requires an absent `description`.
	pub fn new(catalog: &hang::catalog::AudioConfig, config: &Config) -> Result<Self, Error> {
		let name = match &catalog.codec {
			hang::catalog::AudioCodec::Opus => "opus",
			hang::catalog::AudioCodec::Pcm => "pcm",
			#[cfg(feature = "aac")]
			hang::catalog::AudioCodec::AAC(_) => "aac",
			codec => return Err(Error::Unsupported(format!("unsupported audio codec: {codec}"))),
		};
		match &config.kind {
			Kind::Auto | Kind::Software => {}
			Kind::Named(requested) if requested == name => {}
			Kind::Named(requested) => {
				return Err(Error::Unsupported(format!(
					"audio decoder backend {requested:?} is unavailable for {name}"
				)));
			}
		}
		match &catalog.codec {
			hang::catalog::AudioCodec::Opus => Self::new_opus(catalog),
			hang::catalog::AudioCodec::Pcm => Self::new_pcm(catalog),
			#[cfg(feature = "aac")]
			hang::catalog::AudioCodec::AAC(aac) => Self::new_aac(catalog, aac.profile),
			codec => Err(Error::Unsupported(format!("unsupported audio codec: {codec}"))),
		}
	}

	fn new_opus(catalog: &hang::catalog::AudioConfig) -> Result<Self, Error> {
		let head = opus::head(catalog)?;
		let channels = opus::validate_channels(head.channel_count)?;

		let mut err = 0i32;
		// SAFETY: out-pointer is valid; inner is checked for null below.
		let inner = unsafe { opus_decoder_create(opus::DECODE_RATE as i32, channels, &mut err) };
		if err != OPUS_OK || inner.is_null() {
			return Err(opus::error(err, "opus_decoder_create"));
		}
		// Owned before the gain ctl so a failure there still destroys the decoder.
		let decoder = Opus {
			inner,
			pre_skip_remaining: head.pre_skip as usize,
			max_frame_size: (opus::DECODE_RATE as usize * MAX_FRAME_MS) / 1000,
			in_dtx: false,
		};

		// Same Q7.8 dB units as OpusHead, and it survives OPUS_RESET_STATE.
		// SAFETY: `inner` owns a live decoder and OPUS_SET_GAIN takes one i32.
		let rc =
			unsafe { opus_decoder_ctl_impl(decoder.inner, OPUS_SET_GAIN_REQUEST, varargs![head.output_gain as i32]) };
		if rc != OPUS_OK {
			return Err(opus::error(rc, "OPUS_SET_GAIN"));
		}

		Ok(Self {
			delay: decoder.pre_skip_remaining,
			backend: Backend::Opus(decoder),
			sample_rate: opus::DECODE_RATE,
			layout: Layout::from_channels(head.channel_count)?,
		})
	}

	/// AAC-LC only, which is what every gateway that feeds this crate publishes.
	///
	/// HE-AAC is rejected however its config spells it: leading with SBR or PS
	/// (mp4a.40.5 / .29), or leading with LC and declaring SBR in a sync extension
	/// after the core. Symphonia decodes no SBR either way, so the alternative is
	/// half-rate audio that sounds like a fault rather than an unsupported codec.
	/// A stream that signals SBR only in band is indistinguishable from LC in the
	/// config, and does decode as the core.
	#[cfg(feature = "aac")]
	fn new_aac(catalog: &hang::catalog::AudioConfig, profile: u8) -> Result<Self, Error> {
		use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
		use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};

		let description = aac::description(catalog, profile)?;

		let mut params = AudioCodecParameters::new();
		params
			.for_codec(CODEC_ID_AAC)
			.with_extra_data(description.to_vec().into_boxed_slice());

		let inner = symphonia_codec_aac::AacDecoder::try_new(&params, &AudioDecoderOptions::default())
			.map_err(|err| Error::Unsupported(format!("aac decoder: {err}")))?;

		// Resolved by the decoder from the config, so this is what it will emit
		// even when the catalog's own fields say otherwise.
		let params = inner.codec_params();
		let sample_rate = params
			.sample_rate
			.ok_or_else(|| Error::Unsupported("aac config declares no sample rate".into()))?;
		let channel_count = params
			.channels
			.as_ref()
			.map(|channels| channels.count())
			.ok_or_else(|| Error::Unsupported("aac config declares no channels".into()))?;

		Ok(Self {
			backend: Backend::Aac(Box::new(Aac { inner })),
			sample_rate,
			layout: Layout::from_channels(channel_count as u32)?,
			delay: 0,
		})
	}

	fn new_pcm(catalog: &hang::catalog::AudioConfig) -> Result<Self, Error> {
		if catalog.sample_rate == 0 {
			return Err(Error::Unsupported("pcm sample rate must be greater than zero".into()));
		}
		if catalog.channel_count == 0 {
			return Err(Error::Unsupported("pcm channel count must be greater than zero".into()));
		}
		if catalog.description.is_some() {
			return Err(Error::Unsupported("pcm catalog description must be absent".into()));
		}
		let bitrate = pcm::bitrate(catalog.sample_rate, catalog.channel_count)?;
		if catalog.bitrate.is_some_and(|declared| declared != bitrate) {
			return Err(Error::Unsupported(format!(
				"pcm catalog bitrate must be {bitrate} bits per second"
			)));
		}
		let bytes_per_frame = pcm::frame_bytes(1, catalog.channel_count)?;

		Ok(Self {
			backend: Backend::Pcm { bytes_per_frame },
			sample_rate: catalog.sample_rate,
			layout: Layout::from_channels(catalog.channel_count)?,
			delay: 0,
		})
	}

	/// The rate the codec decodes at, read from the catalog.
	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	/// The PCM layout decoded from the catalog.
	pub fn layout(&self) -> Layout {
		self.layout
	}

	/// Reset codec history and reapply startup delay for a new discontinuous epoch.
	pub fn reset(&mut self) -> Result<(), Error> {
		self.reset_prediction()?;
		self.reapply_delay();
		Ok(())
	}

	/// Reapply catalog startup delay for a new playhead epoch without resetting codec prediction.
	pub(super) fn reapply_delay(&mut self) {
		if let Backend::Opus(opus) = &mut self.backend {
			opus.pre_skip_remaining = self.delay;
		}
	}

	/// Reset codec prediction after packet loss without reapplying stream startup delay.
	pub(super) fn reset_prediction(&mut self) -> Result<(), Error> {
		match &mut self.backend {
			Backend::Opus(opus) => {
				// SAFETY: `inner` owns a live decoder and OPUS_RESET_STATE takes no arguments.
				let rc = unsafe { opus_decoder_ctl_impl(opus.inner, OPUS_RESET_STATE, varargs![]) };
				if rc != OPUS_OK {
					return Err(crate::opus::error(rc, "OPUS_RESET_STATE"));
				}
				opus.in_dtx = false;
			}
			Backend::Pcm { .. } => {}
			#[cfg(feature = "aac")]
			Backend::Aac(aac) => aac.inner.reset(),
		}
		Ok(())
	}

	/// How much startup delay is still to be trimmed, in native-rate frames.
	///
	/// Trimmed samples are media the packet covered even though nothing came out of
	/// it, so a caller tracking where a packet ends has to add back whatever this
	/// dropped across the call.
	pub(super) fn delay_remaining(&self) -> usize {
		match &self.backend {
			Backend::Opus(opus) => opus.pre_skip_remaining,
			Backend::Pcm { .. } => 0,
			#[cfg(feature = "aac")]
			Backend::Aac(_) => 0,
		}
	}

	/// Decode one packet into interleaved `f32` PCM and report its codec activity.
	///
	/// Empty Opus packets invoke packet-loss concealment. Loss during DTX remains
	/// classified as DTX, while loss during active audio remains active.
	pub fn decode(&mut self, packet: &[u8]) -> Result<Decoded, Error> {
		match &mut self.backend {
			Backend::Opus(opus) => {
				let channels = self.layout.channels() as usize;
				let mut out = vec![0.0f32; opus.max_frame_size * channels];
				// SAFETY: `inner` owns a live OpusDecoder; packet/out slices are
				// bounded by the lengths we pass.
				let samples = unsafe {
					opus_decode_float(
						&mut *opus.inner,
						packet.as_ptr(),
						packet.len() as i32,
						out.as_mut_ptr(),
						opus.max_frame_size as i32,
						0,
					)
				};
				if samples < 0 {
					return Err(crate::opus::decode_error(samples));
				}
				out.truncate(samples as usize * channels);
				let trim_frames = opus.pre_skip_remaining.min(samples as usize);
				if trim_frames > 0 {
					let trim_samples = trim_frames * channels;
					out.copy_within(trim_samples.., 0);
					out.truncate(out.len() - trim_samples);
					opus.pre_skip_remaining -= trim_frames;
				}
				let activity = crate::opus::activity(packet, opus.in_dtx);
				opus.in_dtx = activity.is_dtx();
				Ok(Decoded { samples: out, activity })
			}
			Backend::Pcm { bytes_per_frame } => {
				if packet.is_empty() || !packet.len().is_multiple_of(*bytes_per_frame) {
					return Err(Error::Misaligned {
						got: packet.len(),
						expected: packet.len().max(1).next_multiple_of(*bytes_per_frame),
					});
				}

				let out = packet
					.as_chunks::<{ pcm::BYTES_PER_SAMPLE }>()
					.0
					.iter()
					.map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]))
					.collect();
				Ok(Decoded {
					samples: out,
					activity: Activity::Active,
				})
			}
			#[cfg(feature = "aac")]
			Backend::Aac(aac) => {
				// The packet is a raw AAC frame, not ADTS, so there is nothing to
				// timestamp it with here: the container carries the timestamp and the
				// decoder only reads the payload.
				let packet = symphonia_core::packet::PacketRef::new(
					0,
					symphonia_core::units::Timestamp::ZERO,
					symphonia_core::units::Duration::ZERO,
					packet,
				);

				let decoded = aac
					.inner
					.decode_ref(&packet)
					.map_err(|err| Error::Decode(format!("aac: {err}")))?;

				let mut out = Vec::new();
				decoded.copy_to_vec_interleaved(&mut out);
				Ok(Decoded {
					samples: out,
					activity: Activity::Active,
				})
			}
		}
	}
}

impl Drop for Opus {
	fn drop(&mut self) {
		// SAFETY: `inner` is a live OpusDecoder that nothing else aliases.
		unsafe { opus_decoder_destroy(self.inner) };
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	/// Three consecutive AAC-LC frames of a 440 Hz full-scale sine, mono at
	/// 44.1 kHz, and the AudioSpecificConfig that opens them. Generated with:
	///
	/// ```text
	/// ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=0.2" -af volume=8 -ac 1 -c:a aac -b:a 32k -f adts sine.aac
	/// ```
	///
	/// then stripping the ADTS header off each frame, since the wire carries raw
	/// AAC. These are frames 2 to 4, past the encoder's priming. lavfi's sine is
	/// an eighth of full scale, which is what the volume filter is undoing.
	#[cfg(feature = "aac")]
	const AAC_DESCRIPTION: &[u8] = b"\x12\x08";

	#[cfg(feature = "aac")]
	const AAC_FRAMES: [&[u8]; 3] = [
		b"\x01\x52\xf2\x8b\x1a\xd7\x8e\x7b\xfd\xa7\xef\xe7\xe3\x55\xd3\x4d\x2f\x55\x2e\x47\x1c\x92\x49\x11\x20\x77\x3f\xbe\x74\xdd\x99\xb3\x7b\xfb\x90\xc9\xf0\x61\x9f\xdc\x0c\x9f\x06\x19\xfd\xe1\x1f\x1f\x00\x67\xf7\x03\x87\xc0\x19\xfd\xc0\xc9\xf0\x07",
		b"\x01\x1e\x32\x89\xe2\x9d\x6b\x33\xe7\xff\xe2\xfe\xbf\xfa\xff\xe7\x2f\x8b\xd5\xd5\xe7\x5f\x3f\x59\xeb\xf1\xcb\xba\xa5\x5e\x52\x4a\xbd\x8d\x74\x50\x8c\x08\xa8\xa0\xd4\x51\x40\xa1\x86\x5d\x06\xb4\x6c\x32\xe6\x25\x9a\x66\x75\xcd\xf9\xbf\x6f\x83\xb7\x53\x80",
		b"\x01\x1e\x32\x8a\x22\x7d\x40\x87\x48\xdb\xdf\xff\xf9\x4f\xff\x87\xde\xef\x8b\xeb\x1e\x77\x5d\xfc\x67\x8f\x8c\x77\x8a\xd6\x29\x96\x1f\x29\xe7\x39\xd4\x53\xcf\x3c\xf3\xce\x79\xd4\x27\x9c\xf5\x65\x2a\x9b\xe9\x80\xb7\xba\xa9\xf9\x58\xc7\x3c\x58\x27\x8a\x60\xa1\x57",
	];

	#[cfg(feature = "aac")]
	fn aac_catalog() -> hang::catalog::AudioConfig {
		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AAC { profile: 2 }, 44_100, 1);
		catalog.description = Some(bytes::Bytes::from_static(AAC_DESCRIPTION));
		catalog
	}

	#[cfg(feature = "aac")]
	#[test]
	fn aac_decodes_a_sine() {
		let mut decoder = Decoder::new(&aac_catalog(), &Config::default()).unwrap();
		assert_eq!(decoder.sample_rate(), 44_100);
		assert_eq!(decoder.layout(), Layout::Mono);

		let decoded: Vec<Vec<f32>> = AAC_FRAMES
			.iter()
			.map(|frame| decoder.decode(frame).unwrap().samples)
			.collect();

		// AAC-LC frames are 1024 samples each, whatever the packet size.
		for pcm in &decoded {
			assert_eq!(pcm.len(), 1024);
		}

		// The first frame is missing the previous frame's overlap, so measure the
		// last one. ffmpeg decodes this same frame to 0.744 RMS, near the 0.707 of
		// an ideal full-scale sine.
		let last = decoded.last().unwrap();
		let rms = (last.iter().map(|s| s * s).sum::<f32>() / last.len() as f32).sqrt();
		assert!((0.65..0.8).contains(&rms), "expected a full-scale sine, got {rms} RMS");
	}

	#[cfg(feature = "aac")]
	#[test]
	fn aac_reports_a_truncated_packet_as_decode() {
		let mut decoder = Decoder::new(&aac_catalog(), &Config::default()).unwrap();

		let truncated = &AAC_FRAMES[0][..16];
		assert!(matches!(decoder.decode(truncated), Err(Error::Decode(_))));
	}

	#[cfg(feature = "aac")]
	#[test]
	fn aac_synthesizes_a_missing_description() {
		// An MSF catalog carries the shape in its own fields instead.
		let mut catalog = aac_catalog();
		catalog.description = None;

		let mut decoder = Decoder::new(&catalog, &Config::default()).unwrap();
		assert_eq!(decoder.sample_rate(), 44_100);
		assert_eq!(decoder.decode(AAC_FRAMES[0]).unwrap().samples.len(), 1024);
	}

	/// A packet libopus rejects is that packet's problem, not the
	/// configuration's. The distinction is what lets a consumer drop the frame and
	/// keep the subscription instead of ending the stream over one bad packet.
	#[test]
	fn opus_reports_a_rejected_packet_as_decode() {
		let head = moq_mux::codec::opus::Config::new(48_000, 2).encode().unwrap();
		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		catalog.description = Some(head);

		let mut decoder = Decoder::new(&catalog, &Config::default()).unwrap();

		// Not a valid TOC byte sequence: libopus reports OPUS_INVALID_PACKET.
		assert!(matches!(decoder.decode(&[0xFF; 3]), Err(Error::Decode(_))));
	}

	/// Real Opus: 20 ms packets of a continuous 440 Hz sine, mono, from libopus
	/// with its 312-sample lookahead.
	pub(crate) fn opus_packets(count: usize) -> Vec<bytes::Bytes> {
		let mut encoder = crate::encode::Encoder::new(&crate::encode::Settings::new(48_000, Layout::Mono)).unwrap();
		let frames = encoder.frame_size();
		(0..count)
			.map(|packet| {
				let pcm: Vec<f32> = (packet * frames..(packet + 1) * frames)
					.map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
					.collect();
				encoder.encode(&pcm).unwrap().payload
			})
			.collect()
	}

	/// A catalog shaped like an import's: the OpusHead input rate as the catalog rate.
	pub(crate) fn opus_catalog(head: moq_mux::codec::opus::Config) -> hang::catalog::AudioConfig {
		let mut catalog =
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, head.sample_rate, head.channel_count);
		catalog.description = Some(head.encode().unwrap());
		catalog
	}

	fn rms(samples: &[f32]) -> f32 {
		(samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
	}

	/// The OpusHead input rate is metadata: 44.1 kHz and unknown (0) are valid
	/// heads, and no input rate changes the 48 kHz clock the packets decode on.
	#[test]
	fn opus_decodes_at_48k_whatever_the_input_rate() {
		let packets = opus_packets(2);
		for input_rate in [0, 8_000, 24_000, 44_100, 48_000, 96_000] {
			let head = moq_mux::codec::opus::Config::new(input_rate, 1).with_pre_skip(312);
			let mut decoder = Decoder::new(&opus_catalog(head), &Config::default()).unwrap();
			assert_eq!(decoder.sample_rate(), 48_000, "input rate {input_rate}");

			// The pre-skip is 48 kHz samples, trimmed once.
			assert_eq!(decoder.decode(&packets[0]).unwrap().samples.len(), 960 - 312);
			assert_eq!(decoder.decode(&packets[1]).unwrap().samples.len(), 960);
		}
	}

	#[test]
	fn opus_applies_the_declared_gain() {
		let packets = opus_packets(5);
		let decode = |output_gain: i16| {
			let mut head = moq_mux::codec::opus::Config::new(48_000, 1);
			head.output_gain = output_gain;
			let mut decoder = Decoder::new(&opus_catalog(head), &Config::default()).unwrap();
			let mut last = Vec::new();
			for packet in &packets {
				last = decoder.decode(packet).unwrap().samples;
			}
			// A reset after loss keeps the gain.
			decoder.reset().unwrap();
			let reset = decoder.decode(&packets[4]).unwrap().samples;
			(rms(&last), rms(&reset))
		};

		// -6.02 dB in Q7.8 halves the amplitude.
		let (plain, plain_reset) = decode(0);
		let (quiet, quiet_reset) = decode(-1541);
		assert!((quiet / plain - 0.5).abs() < 0.001, "gain ratio {}", quiet / plain);
		assert!(
			(quiet_reset / plain_reset - 0.5).abs() < 0.001,
			"gain ratio after reset {}",
			quiet_reset / plain_reset
		);
	}

	/// A present description is the stream's configuration, so a broken one is
	/// refused rather than replaced by the catalog's fields.
	#[test]
	fn opus_refuses_a_malformed_description() {
		let valid = moq_mux::codec::opus::Config::new(48_000, 2).encode().unwrap().to_vec();
		let mut version = valid.clone();
		version[8] = 16;
		let mut channels = valid.clone();
		channels[9] = 3;
		let mut signature = valid.clone();
		signature[0] = b'X';
		// Family 1 promising a table that is not there.
		let mut table = valid.clone();
		table[18] = 1;

		for (name, description) in [
			("truncated", valid[..18].to_vec()),
			("empty", Vec::new()),
			("signature", signature),
			("version", version),
			("channels", channels),
			("table", table),
		] {
			let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
			catalog.description = Some(description.into());
			assert!(
				matches!(Decoder::new(&catalog, &Config::default()), Err(Error::Unsupported(_))),
				"{name}"
			);
		}
	}

	/// Mapping families other than 0 need the multistream decoder; a stereo
	/// family 1 head is refused rather than decoded as family 0.
	#[test]
	fn opus_refuses_unsupported_channel_mappings() {
		let valid = moq_mux::codec::opus::Config::new(48_000, 2).encode().unwrap().to_vec();
		for family in [1, 255] {
			let mut description = valid.clone();
			description[18] = family;
			// One coupled stream, left then right.
			description.extend_from_slice(&[1, 1, 0, 1]);

			let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
			catalog.description = Some(description.into());
			assert!(
				matches!(Decoder::new(&catalog, &Config::default()), Err(Error::Unsupported(_))),
				"family {family}"
			);
		}
	}

	/// Without a description the catalog shapes the stream, which has no pre-skip.
	#[test]
	fn opus_decodes_without_a_description() {
		let packets = opus_packets(1);
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 24_000, 1);
		let mut decoder = Decoder::new(&catalog, &Config::default()).unwrap();
		assert_eq!(decoder.sample_rate(), 48_000);
		assert_eq!(decoder.decode(&packets[0]).unwrap().samples.len(), 960);

		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 6);
		assert!(matches!(
			Decoder::new(&catalog, &Config::default()),
			Err(Error::Unsupported(_))
		));
	}

	#[test]
	fn pcm_rejects_incomplete_channel_frame() {
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 2);
		let mut decoder = Decoder::new(&catalog, &Config::default()).unwrap();

		assert!(matches!(
			decoder.decode(&[]),
			Err(Error::Misaligned { got: 0, expected: 8 })
		));
		assert!(matches!(
			decoder.decode(&[0; 4]),
			Err(Error::Misaligned { got: 4, expected: 8 })
		));
	}

	#[test]
	fn decoder_rejects_unknown_codec() {
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Unknown("future".into()), 48_000, 2);

		assert!(matches!(
			Decoder::new(&catalog, &Config::default()),
			Err(Error::Unsupported(_))
		));
	}

	#[test]
	fn pcm_rejects_incorrect_catalog_bitrate() {
		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 2);
		catalog.bitrate = Some(1);

		assert!(matches!(
			Decoder::new(&catalog, &Config::default()),
			Err(Error::Unsupported(_))
		));
	}

	#[test]
	fn refuses_unavailable_backend() {
		let catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 2);
		let config = Config {
			kind: Kind::Named("missing".into()),
		};
		assert!(matches!(Decoder::new(&catalog, &config), Err(Error::Unsupported(_))));
	}
}

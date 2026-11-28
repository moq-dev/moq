//! The original hang wire format.
//!
//! Each moq frame holds one media frame: a varint-encoded timestamp
//! followed by the raw codec bitstream. Simple but ad-hoc; new
//! broadcasts should use [`crate::container::loc`] instead.

use crate::container::{Container, Frame, Kind};

/// Hang Legacy wire format configured for the track's media role.
pub struct Wire(
	/// The kind of content carried by the track.
	pub Kind,
);

impl Wire {
	/// The bytes preceding a frame's payload: its varint timestamp.
	pub(crate) fn prefix(&self, timestamp: moq_net::Timestamp) -> crate::Result<bytes::Bytes> {
		let mut prefix = bytes::BytesMut::new();
		hang::container::Frame {
			timestamp,
			payload: bytes::Bytes::new(),
		}
		.encode(&mut prefix)?;
		Ok(prefix.freeze())
	}
}

impl Container for Wire {
	type Error = crate::Error;

	fn end(&self, frame: &Frame) -> Option<moq_net::Timestamp> {
		(self.0 != Kind::Data && frame.payload.is_empty()).then_some(frame.timestamp)
	}

	fn kind(&self) -> Kind {
		self.0
	}

	fn finish_group(
		&self,
		group: &mut moq_net::group::Producer,
		end: Option<moq_net::Timestamp>,
	) -> Result<(), Self::Error> {
		if self.0 == Kind::Video
			&& let Some(timestamp) = end
		{
			hang::container::Frame {
				timestamp,
				payload: bytes::Bytes::new(),
			}
			.write_to(group)?;
		}
		Ok(())
	}

	fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
		for frame in frames {
			let hang_frame = hang::container::Frame {
				timestamp: frame.timestamp,
				payload: frame.payload.clone(),
			};
			hang_frame.write_to(group)?;
		}
		Ok(())
	}

	fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
		// On a data track an object with no bytes is its discontinuity marker, not a frame.
		// Elsewhere it is malformed, so it fails to decode below.
		if self.0 == Kind::Data && frame.payload.is_empty() {
			return Ok(Vec::new());
		}

		let hang_frame = hang::container::Frame::decode(frame.payload)?;
		Ok(vec![Frame {
			timestamp: hang_frame.timestamp,
			payload: hang_frame.payload,
			// Legacy doesn't carry the keyframe bit on the wire; the
			// wrapping Consumer fills it in from group position.
			keyframe: false,
			// Legacy carries no per-frame duration.
			duration: None,
		}])
	}
}

/// How long each packet of a Legacy audio track plays, as its codec states.
///
/// Legacy frames carry no duration; the hang draft leaves it to the codec. Knowing it, a reader
/// tells a group that ends where the next one starts from one followed by lost audio. Group keys
/// can't say: they are starts, and a wire without skips (moq-lite-05 and 06) reports the starts
/// between two groups as it does a loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Audio(Packets);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Packets {
	/// Read from each packet's TOC byte, in 48 kHz samples.
	Opus,
	/// The same number of samples in every packet, at the codec's rate: AAC's core may run below
	/// the track's.
	Fixed { samples: u32, rate: u32 },
	/// Interleaved 32-bit samples, so the payload's length says.
	Pcm { frame_bytes: u32, rate: u32 },
}

impl Audio {
	/// The packet clock of `config`'s codec, or `None` when a packet doesn't state its duration
	/// without decoding it.
	pub fn new(config: &hang::catalog::AudioConfig) -> Option<Self> {
		let rate = config.sample_rate;
		if rate == 0 {
			return None;
		}
		let packets = match &config.codec {
			hang::catalog::AudioCodec::Opus => Packets::Opus,
			// AAC codes 960 or 1024 samples per packet at its core's rate, which SBR and PS may
			// double on output. The AudioSpecificConfig names both; without one, only AAC-LC's
			// rate is the catalog's, at the usual 1024.
			hang::catalog::AudioCodec::AAC(aac) => {
				let (samples, rate) = match &config.description {
					Some(asc) => crate::codec::aac::frame_clock(asc),
					None => (aac.profile == 2).then_some((1024, rate)),
				}?;
				Packets::Fixed { samples, rate }
			}
			// Layer II codes 1152 samples per frame.
			hang::catalog::AudioCodec::Mp2 => Packets::Fixed { samples: 1152, rate },
			hang::catalog::AudioCodec::Pcm => Packets::Pcm {
				frame_bytes: config.channel_count.checked_mul(4).filter(|bytes| *bytes > 0)?,
				rate,
			},
			_ => return None,
		};
		Some(Self(packets))
	}

	/// How long `payload` plays, or `None` when it doesn't parse as one packet.
	pub fn duration(&self, payload: &[u8]) -> Option<moq_net::Timestamp> {
		if payload.is_empty() {
			return None;
		}
		let (samples, rate) = match self.0 {
			Packets::Opus => (crate::codec::opus::packet_samples(payload)?, 48_000),
			Packets::Fixed { samples, rate } => (samples, rate),
			Packets::Pcm { frame_bytes, rate } => {
				let frame_bytes = frame_bytes as usize;
				if !payload.len().is_multiple_of(frame_bytes) {
					return None;
				}
				(u32::try_from(payload.len() / frame_bytes).ok()?, rate)
			}
		};
		moq_net::Timestamp::from_scale(samples.into(), rate.into()).ok()
	}

	/// Decode a frame as [`Wire`] does for audio, stating each packet's duration.
	pub(crate) fn decode(&self, frame: moq_net::frame::Frame) -> crate::Result<Vec<Frame>> {
		let mut frames = Wire(Kind::Audio).decode(frame)?;
		for frame in &mut frames {
			frame.duration = self.duration(&frame.payload);
		}
		Ok(frames)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn config(codec: hang::catalog::AudioCodec, sample_rate: u32, channel_count: u32) -> hang::catalog::AudioConfig {
		hang::catalog::AudioConfig::new(codec, sample_rate, channel_count)
	}

	fn micros(duration: Option<moq_net::Timestamp>) -> Option<u128> {
		duration.map(|duration| std::time::Duration::try_from(duration).unwrap().as_micros())
	}

	#[test]
	fn opus_states_each_packet_in_its_toc() {
		let audio = Audio::new(&config(hang::catalog::AudioCodec::Opus, 48_000, 2)).unwrap();
		// CELT FB at 2.5 ms and 20 ms, one frame each.
		assert_eq!(micros(audio.duration(&[16 << 3, 0])), Some(2_500));
		assert_eq!(micros(audio.duration(&[31 << 3, 0])), Some(20_000));
		// Two 20 ms frames.
		assert_eq!(micros(audio.duration(&[(31 << 3) | 1, 0])), Some(40_000));
		// An empty payload is a marker, not a packet.
		assert_eq!(audio.duration(&[]), None);
	}

	#[test]
	fn aac_lc_mp2_and_pcm_state_a_fixed_or_measured_count() {
		let aac = hang::catalog::AudioCodec::AAC(hang::catalog::AAC { profile: 2 });
		let audio = Audio::new(&config(aac, 48_000, 2)).unwrap();
		assert_eq!(micros(audio.duration(&[0xff])), Some(21_333));

		let audio = Audio::new(&config(hang::catalog::AudioCodec::Mp2, 48_000, 2)).unwrap();
		assert_eq!(micros(audio.duration(&[0xff])), Some(24_000));

		let audio = Audio::new(&config(hang::catalog::AudioCodec::Pcm, 1_000, 2)).unwrap();
		// Ten stereo frames of 32-bit samples are 10 ms at 1 kHz.
		assert_eq!(micros(audio.duration(&[0; 80])), Some(10_000));
		assert_eq!(audio.duration(&[0; 81]), None, "a partial sample frame");
	}

	#[test]
	fn a_codec_whose_packets_vary_states_nothing() {
		let he = hang::catalog::AudioCodec::AAC(hang::catalog::AAC { profile: 5 });
		assert_eq!(Audio::new(&config(he, 48_000, 2)), None);
	}

	#[test]
	fn aac_states_its_configs_frame_length_at_its_core_rate() {
		let lc = hang::catalog::AudioCodec::AAC(hang::catalog::AAC { profile: 2 });
		let he = hang::catalog::AudioCodec::AAC(hang::catalog::AAC { profile: 5 });
		for (codec, sample_rate, asc, duration) in [
			// A 48 kHz core with SBR at 96 kHz behind a sync extension, then at the core's own rate.
			(lc.clone(), 96_000, &[0x11, 0x90, 0x56, 0xe5, 0x80][..], 21_333),
			(lc.clone(), 48_000, &[0x11, 0x90, 0x56, 0xe5, 0x98][..], 21_333),
			// Plain AAC-LC, and explicit SBR over a 48 kHz core at 96 kHz.
			(lc.clone(), 48_000, &[0x11, 0x90][..], 21_333),
			(he.clone(), 96_000, &[0x29, 0x90, 0x08, 0x00][..], 21_333),
			// frameLengthFlag set: 960 samples, plain and under explicit SBR over a 24 kHz core.
			(lc, 48_000, &[0x11, 0x94][..], 20_000),
			(he, 48_000, &[0x2b, 0x11, 0x8a, 0x00][..], 40_000),
		] {
			let mut aac = config(codec, sample_rate, 2);
			aac.description = Some(bytes::Bytes::copy_from_slice(asc));
			let audio = Audio::new(&aac).unwrap();
			assert_eq!(micros(audio.duration(&[0xff])), Some(duration), "{asc:02x?}");
		}
	}
}

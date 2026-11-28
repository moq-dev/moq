//! Opus through libopus, the software decoder for every host.
//!
//! Every stream goes through the multistream decoder, which is also how a
//! family 0 (mono/stereo) stream decodes: one stream, coupled when stereo.

use std::collections::VecDeque;

use super::Backend;
use crate::decode::Decoded;
use crate::{Error, Layout, opus};

pub(super) const NAME: &str = "libopus";

/// Opus packets cap at 120 ms (RFC 6716 §2.1.4).
const MAX_FRAME_MS: usize = 120;

pub(super) struct Libopus {
	inner: moq_opus::Decoder,
	layout: Layout,
	/// For each canonical channel, the Vorbis-order channel it comes from, when they differ.
	reorder: Option<&'static [usize]>,
	/// Opus streams in one packet. Family 0 is one; the rest come from the mapping.
	streams: u8,
	pre_skip: usize,
	max_frame_size: usize,
	/// Frames per channel in the last packet that decoded, which is what a lost
	/// packet most likely carried. `None` until one has since the last reset.
	last_frame_size: Option<usize>,
	in_dtx: bool,
	/// Outputs submitted but not yet received, which reuse their slots across calls.
	ready: VecDeque<Decoded>,
}

impl Libopus {
	/// Decodes at 48 kHz with the pre-skip and output gain the OpusHead
	/// `description` declares; its input rate is metadata. A missing description
	/// falls back to the catalog's channel count, which must then be mono or
	/// stereo. A description that does not parse is refused: guessing family 0
	/// would decode those packets with the wrong stream layout.
	///
	/// Channel mapping family 1 decodes up to 7.1 in the canonical [`Layout`]
	/// order; every other family is refused, since none declares speakers.
	pub(super) fn open(catalog: &hang::catalog::AudioConfig) -> Result<Box<dyn Backend>, Error> {
		let head = opus::head(catalog)?;
		let channel_count = head.channel_count;

		let (mapping, layout, reorder) = match &head.mapping {
			None => {
				let mapping = moq_opus::Mapping::family0(opus::validate_channels(channel_count)?)
					.map_err(|err| opus::error(err, "create"))?;
				(mapping, Layout::from_channels(channel_count)?, None)
			}
			Some(mapping) if mapping.family() == 1 => {
				let (layout, reorder) = vorbis(channel_count)?;
				let mapping = moq_opus::Mapping {
					streams: mapping.streams(),
					coupled: mapping.coupled(),
					table: mapping.table().to_vec(),
				};
				(mapping, layout, reorder)
			}
			Some(mapping) => {
				return Err(Error::Unsupported(format!(
					"opus channel mapping family {} declares no speaker positions",
					mapping.family()
				)));
			}
		};

		let mut inner =
			moq_opus::Decoder::new(opus::DECODE_RATE, &mapping).map_err(|err| opus::error(err, "create"))?;
		// Same Q7.8 dB units as OpusHead, and it survives a reset.
		inner
			.set_gain(head.output_gain)
			.map_err(|err| opus::error(err, "set gain"))?;

		Ok(Box::new(Self {
			inner,
			layout,
			reorder,
			streams: mapping.streams,
			pre_skip: head.pre_skip as usize,
			max_frame_size: (opus::DECODE_RATE as usize * MAX_FRAME_MS) / 1000,
			last_frame_size: None,
			in_dtx: false,
			ready: VecDeque::new(),
		}))
	}
}

/// The layout a family 1 stream of `channels` declares (RFC 7845 §5.1.1.2), and
/// how to reorder its Vorbis channel order into that layout's canonical one.
///
/// Vorbis puts the center between the fronts and the LFE last. Its "rear"
/// pair in 5.0 and 5.1 is the surround pair, which is side in the canonical
/// layouts, while 7.1 has distinct side and rear pairs.
fn vorbis(channels: u32) -> Result<(Layout, Option<&'static [usize]>), Error> {
	Ok(match channels {
		1 => (Layout::Mono, None),
		2 => (Layout::Stereo, None),
		// L, C, R.
		3 => (Layout::ThreePointZero, Some(&[0, 2, 1])),
		// FL, FR, RL, RR.
		4 => (Layout::Quad, None),
		// FL, C, FR, RL, RR.
		5 => (Layout::FivePointZero, Some(&[0, 2, 1, 3, 4])),
		// FL, C, FR, RL, RR, LFE.
		6 => (Layout::FivePointOne, Some(&[0, 2, 1, 5, 3, 4])),
		// FL, C, FR, SL, SR, RC, LFE.
		7 => (Layout::SixPointOne, Some(&[0, 2, 1, 6, 5, 3, 4])),
		// FL, C, FR, SL, SR, RL, RR, LFE.
		8 => (Layout::SevenPointOne, Some(&[0, 2, 1, 7, 5, 6, 3, 4])),
		other => {
			return Err(Error::Unsupported(format!(
				"opus channel mapping family 1 has no {other}-channel layout"
			)));
		}
	})
}

impl Backend for Libopus {
	/// An empty packet conceals as much audio as the last packet that decoded
	/// held, in whichever mode (SILK, hybrid, or CELT) that packet used, and is
	/// refused before any has. Loss during DTX remains classified as DTX, while
	/// loss during active audio remains active.
	fn decode(&mut self, packet: &[u8]) -> Result<(), Error> {
		let channels = self.layout.channels() as usize;
		// libopus conceals exactly `frame_size` samples for a lost packet, and
		// the decoder can't know how long it was, so assume the last one's length.
		// A real packet decodes into the most it can hold.
		let frame_size = if packet.is_empty() {
			self.last_frame_size.ok_or_else(|| {
				Error::Decode("opus packet lost before any packet decoded, so no length to conceal".into())
			})?
		} else {
			self.max_frame_size
		};
		let mut out = vec![0.0f32; frame_size * channels];
		let samples = self.inner.decode_float(packet, &mut out).map_err(opus::decode_error)?;
		if !packet.is_empty() {
			self.last_frame_size = Some(samples);
		}
		out.truncate(samples * channels);

		if let Some(order) = self.reorder {
			let mut vorbis = [0.0f32; 8];
			for frame in out.chunks_exact_mut(channels) {
				vorbis[..channels].copy_from_slice(frame);
				for (sample, &from) in frame.iter_mut().zip(order) {
					*sample = vorbis[from];
				}
			}
		}

		let activity = opus::multistream_activity(packet, self.streams, self.in_dtx);
		self.in_dtx = activity.is_dtx();
		self.ready.push_back(Decoded { samples: out, activity });
		Ok(())
	}

	fn receive(&mut self) -> Result<Option<Decoded>, Error> {
		Ok(self.ready.pop_front())
	}

	/// libopus readies each packet's samples as soon as it is submitted.
	fn flush(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn reset(&mut self) -> Result<(), Error> {
		self.inner.reset().map_err(|err| opus::error(err, "reset"))?;
		self.in_dtx = false;
		// A reset decoder has no history to conceal from.
		self.last_frame_size = None;
		self.ready.clear();
		Ok(())
	}

	fn conceals(&self) -> bool {
		self.last_frame_size.is_some()
	}

	fn sample_rate(&self) -> u32 {
		opus::DECODE_RATE
	}

	fn layout(&self) -> Layout {
		self.layout
	}

	fn delay(&self) -> usize {
		self.pre_skip
	}

	fn name(&self) -> &str {
		NAME
	}
}

#[cfg(test)]
mod tests {
	use crate::Error;
	use crate::decode::{Config, Decoder};
	use crate::layout::Speaker::{self, *};

	/// Speakers in Vorbis channel order for each family 1 channel count (RFC 7845 §5.1.1.2).
	const VORBIS: [&[Speaker]; 8] = [
		&[FrontCenter],
		&[FrontLeft, FrontRight],
		&[FrontLeft, FrontCenter, FrontRight],
		&[FrontLeft, FrontRight, BackLeft, BackRight],
		&[FrontLeft, FrontCenter, FrontRight, SideLeft, SideRight],
		&[FrontLeft, FrontCenter, FrontRight, SideLeft, SideRight, Lfe],
		&[FrontLeft, FrontCenter, FrontRight, SideLeft, SideRight, BackCenter, Lfe],
		&[
			FrontLeft,
			FrontCenter,
			FrontRight,
			SideLeft,
			SideRight,
			BackLeft,
			BackRight,
			Lfe,
		],
	];

	const RATE: usize = 48_000;
	const FRAME: usize = 960;
	const PACKETS: usize = 15;

	/// A distinct tone per speaker, low for the LFE, which libopus band-limits.
	fn tone(speaker: Speaker) -> f32 {
		match speaker {
			Lfe => 80.0,
			other => 400.0 + 300.0 * other as u8 as f32,
		}
	}

	/// The family 1 streams, coupled streams, and mapping table for each channel
	/// count (RFC 7845 §5.1.1.2), as libopus's surround encoder lays them out.
	const MAPPINGS: [(u8, u8, &[u8]); 8] = [
		(1, 0, &[0]),
		(1, 1, &[0, 1]),
		(2, 1, &[0, 2, 1]),
		(2, 2, &[0, 1, 2, 3]),
		(3, 2, &[0, 4, 1, 2, 3]),
		(4, 2, &[0, 4, 1, 2, 3, 5]),
		(4, 3, &[0, 4, 1, 2, 3, 5, 6]),
		(5, 3, &[0, 6, 1, 2, 3, 4, 5, 7]),
	];

	/// Encode `channels` of Vorbis-ordered tones as a family 1 stream, returning
	/// its OpusHead and packets.
	///
	/// Each stream is its own mono or stereo encoder, and each packet joins their
	/// packets, every one but the last self-delimited (RFC 6716 Appendix B).
	fn surround(channels: usize) -> (bytes::Bytes, Vec<Vec<u8>>) {
		let speakers = VORBIS[channels - 1];
		let (streams, coupled, table) = MAPPINGS[channels - 1];
		// The input channels each stream codes: the left and right of a coupled
		// stream, or the one channel of a mono stream.
		let inputs: Vec<Vec<usize>> = (0..streams)
			.map(|stream| {
				let decoded: Vec<u8> = match stream < coupled {
					true => vec![2 * stream, 2 * stream + 1],
					false => vec![coupled + stream],
				};
				let channel = |index| table.iter().position(|&entry| entry == index).unwrap();
				decoded.into_iter().map(channel).collect()
			})
			.collect();
		let mut encoders: Vec<_> = inputs
			.iter()
			.map(|input| moq_opus::Encoder::new(RATE as u32, input.len() as u8, moq_opus::Application::Audio).unwrap())
			.collect();

		let packets = (0..PACKETS)
			.map(|packet| {
				let mut joined = Vec::new();
				for (index, (encoder, input)) in encoders.iter_mut().zip(&inputs).enumerate() {
					let mut pcm = Vec::with_capacity(FRAME * input.len());
					for i in packet * FRAME..(packet + 1) * FRAME {
						for &channel in input {
							let phase = std::f32::consts::TAU * tone(speakers[channel]) * i as f32 / RATE as f32;
							pcm.push(phase.sin() * 0.5);
						}
					}
					let mut out = [0u8; 1500];
					let len = encoder.encode_float(&pcm, &mut out).unwrap();
					let (toc, frame) = (out[0], &out[1..len]);
					joined.push(toc);
					if index + 1 < inputs.len() {
						// Self-delimiting a one-frame packet puts the frame's length after the TOC.
						assert_eq!(toc & 0x3, 0, "a 20 ms packet holds one frame");
						match frame.len() {
							len @ 0..252 => joined.push(len as u8),
							len => {
								let first = 252 + (len & 0x3);
								joined.extend_from_slice(&[first as u8, ((len - first) >> 2) as u8]);
							}
						}
					}
					joined.extend_from_slice(frame);
				}
				joined
			})
			.collect();

		let mut head = moq_mux::codec::opus::Config::new(RATE as u32, 2)
			.encode()
			.unwrap()
			.to_vec();
		head[9] = channels as u8;
		head[18] = 1;
		head.extend_from_slice(&[streams, coupled]);
		head.extend_from_slice(table);
		(head.into(), packets)
	}

	fn catalog(head: bytes::Bytes, channels: u32) -> hang::catalog::AudioConfig {
		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, channels);
		catalog.description = Some(head);
		catalog
	}

	/// Energy of `samples` at `freq`, by the Goertzel recurrence.
	fn power(samples: &[f32], freq: f32) -> f32 {
		let coeff = 2.0 * (std::f32::consts::TAU * freq / RATE as f32).cos();
		let (mut s1, mut s2) = (0.0f32, 0.0f32);
		for &x in samples {
			let s = x + coeff * s1 - s2;
			s2 = s1;
			s1 = s;
		}
		s1 * s1 + s2 * s2 - coeff * s1 * s2
	}

	/// Every family 1 layout decodes with each canonical channel carrying its
	/// own speaker's tone, which pins the Vorbis reorder independently of it.
	#[test]
	fn family_one_decodes_in_canonical_order() {
		for channels in 1..=8 {
			let (head, packets) = surround(channels);
			let mut decoder = Decoder::new(&catalog(head, channels as u32), &Config::default()).unwrap();
			let layout = decoder.layout();
			// The layout has exactly the Vorbis speakers, which canonical order sorts.
			let mut speakers = VORBIS[channels - 1].to_vec();
			speakers.sort_by_key(|&speaker| speaker as u8);
			assert_eq!(layout.speakers().unwrap(), speakers, "{channels} channels");

			let mut pcm = Vec::new();
			for packet in &packets {
				pcm.extend(crate::single(decoder.decode_all(packet).unwrap()).samples);
			}
			// Skip the encoder's warmup.
			let pcm = &pcm[pcm.len() / 2..];

			let tones: Vec<f32> = VORBIS[channels - 1].iter().copied().map(tone).collect();
			for (index, &speaker) in layout.speakers().unwrap().iter().enumerate() {
				let channel: Vec<f32> = pcm.iter().skip(index).step_by(channels).copied().collect();
				let loudest = tones
					.iter()
					.copied()
					.max_by(|a, b| power(&channel, *a).total_cmp(&power(&channel, *b)))
					.unwrap();
				assert_eq!(loudest, tone(speaker), "{channels} channels, {speaker:?} at {index}");
			}
		}
	}

	/// An all-DTX surround packet is one empty Opus packet per stream. Read as a
	/// single stream, the later subpackets look like payload and the span is lost.
	#[test]
	fn surround_silence_is_dtx() {
		let (head, packets) = surround(6);
		let mut decoder = Decoder::new(&catalog(head.clone(), 6), &Config::default()).unwrap();
		let mid = crate::single(decoder.decode_all(&packets[PACKETS / 2]).unwrap());
		assert!(mid.activity.is_active(), "a coded surround frame must stay active");

		// Two coupled streams, then two mono, each a 20 ms empty frame.
		let dtx = [0xfc, 0x00, 0xfc, 0x00, 0xf8, 0x00, 0xf8];
		let mut decoder = Decoder::new(&catalog(head, 6), &Config::default()).unwrap();
		let decoded = crate::single(decoder.decode_all(&dtx).expect("empty multistream packet"));
		assert!(decoded.activity.is_dtx(), "all-DTX surround packet read as active");
		assert_eq!(decoded.samples.len() % 6, 0);
	}

	/// A description that is present but truncated used to be dropped, and a
	/// stereo catalog then opened a family 0 decoder for a family 1 or 255 head.
	#[test]
	fn malformed_description_is_refused() {
		for family in [1u8, 255] {
			let mut head = moq_mux::codec::opus::Config::new(48_000, 2).encode().unwrap().to_vec();
			head[18] = family;
			let err = Decoder::new(&catalog(head.into(), 2), &Config::default())
				.err()
				.expect("refused");
			assert!(
				matches!(&err, Error::Unsupported(message) if message.contains("opus description")),
				"family {family}: {err}"
			);
		}

		let plain = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		assert!(Decoder::new(&plain, &Config::default()).is_ok());
	}

	/// Families other than 0 and 1 carry no speaker positions, so there is
	/// nothing to put in a layout: refused, not passed through as discrete.
	#[test]
	fn other_families_are_refused() {
		for (family, channels, table) in [(255u8, 2u8, &[2, 0, 0, 1][..]), (2, 4, &[4, 0, 0, 1, 2, 3])] {
			let mut head = moq_mux::codec::opus::Config::new(48_000, 2).encode().unwrap().to_vec();
			head[9] = channels;
			head[18] = family;
			head.extend_from_slice(table);

			let err = Decoder::new(&catalog(head.into(), channels.into()), &Config::default())
				.err()
				.expect("refused");
			assert!(
				matches!(&err, Error::Unsupported(message) if message.contains("no speaker positions")),
				"family {family}: {err}"
			);
		}
	}
}

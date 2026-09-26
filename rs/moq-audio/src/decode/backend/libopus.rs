//! Opus through libopus, the software decoder for every host.
//!
//! Every stream goes through the multistream decoder, which is also how a
//! family 0 (mono/stereo) stream decodes: one stream, coupled when stereo.

use unsafe_libopus::{
	OPUS_OK, OPUS_RESET_STATE, OpusMSDecoder, opus_multistream_decode_float, opus_multistream_decoder_create,
	opus_multistream_decoder_ctl_impl, opus_multistream_decoder_destroy, varargs,
};

use super::Backend;
use crate::decode::Decoded;
use crate::{Error, Layout, opus};

pub(super) const NAME: &str = "libopus";

/// Opus packets cap at 120 ms (RFC 6716 §2.1.4).
const MAX_FRAME_MS: usize = 120;

/// The family 0 streams: one stream, coupled when stereo, decoded in order.
const MONO: &[u8] = &[0];
const STEREO: &[u8] = &[0, 1];

pub(super) struct Libopus {
	inner: *mut OpusMSDecoder,
	sample_rate: u32,
	layout: Layout,
	/// For each canonical channel, the Vorbis-order channel it comes from, when they differ.
	reorder: Option<&'static [usize]>,
	/// Opus streams in one packet. Family 0 is one; the rest come from the mapping.
	streams: u8,
	pre_skip: usize,
	max_frame_size: usize,
	in_dtx: bool,
}

// SAFETY: the decoder is owned exclusively and libopus keeps no thread-local state.
unsafe impl Send for Libopus {}

impl Libopus {
	/// Parses the OpusHead `description` when one is present. A missing description
	/// falls back to the catalog's sample rate and channel count, which must then
	/// be mono or stereo. A description that does not parse is refused: guessing
	/// family 0 would decode those packets with the wrong stream layout.
	///
	/// Channel mapping family 1 decodes up to 7.1 in the canonical [`Layout`]
	/// order; every other family is refused, since none declares speakers.
	pub(super) fn open(catalog: &hang::catalog::AudioConfig) -> Result<Box<dyn Backend>, Error> {
		let head = match catalog.description.as_ref() {
			Some(desc) => moq_mux::codec::opus::Config::parse(&mut desc.as_ref())
				.map_err(|err| Error::Unsupported(format!("opus description: {err}")))?,
			None => moq_mux::codec::opus::Config::new(catalog.sample_rate, catalog.channel_count),
		};
		let (sample_rate, channel_count, pre_skip) = (head.sample_rate, head.channel_count, head.pre_skip);

		opus::validate_rate(sample_rate)?;
		let (streams, coupled, table, layout, reorder) = match &head.mapping {
			None => {
				let table = match opus::validate_channels(channel_count)? {
					1 => MONO,
					_ => STEREO,
				};
				let layout = Layout::from_channels(channel_count)?;
				(1, table.len() as i32 - 1, table, layout, None)
			}
			Some(mapping) if mapping.family() == 1 => {
				let (layout, reorder) = vorbis(channel_count)?;
				let (streams, coupled) = (mapping.streams() as i32, mapping.coupled() as i32);
				(streams, coupled, mapping.table(), layout, reorder)
			}
			Some(mapping) => {
				return Err(Error::Unsupported(format!(
					"opus channel mapping family {} declares no speaker positions",
					mapping.family()
				)));
			}
		};

		let mut err = 0i32;
		// SAFETY: `table` holds one entry per output channel, the count we pass,
		// and the out-pointer is valid; inner is checked for null below.
		let inner = unsafe {
			opus_multistream_decoder_create(
				sample_rate as i32,
				table.len() as i32,
				streams,
				coupled,
				table.as_ptr(),
				&mut err,
			)
		};
		if err != OPUS_OK || inner.is_null() {
			return Err(opus::error(err, "opus_multistream_decoder_create"));
		}

		Ok(Box::new(Self {
			inner,
			sample_rate,
			layout,
			reorder,
			streams: streams as u8,
			// OpusHead counts pre-skip at 48 kHz whatever rate the decoder runs at.
			pre_skip: (pre_skip as usize * sample_rate as usize) / 48_000,
			max_frame_size: (sample_rate as usize * MAX_FRAME_MS) / 1000,
			in_dtx: false,
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
	/// Empty packets invoke packet-loss concealment. Loss during DTX remains
	/// classified as DTX, while loss during active audio remains active.
	fn decode(&mut self, packet: &[u8]) -> Result<Decoded, Error> {
		let channels = self.layout.channels() as usize;
		let mut out = vec![0.0f32; self.max_frame_size * channels];
		// SAFETY: `inner` owns a live OpusMSDecoder; packet/out slices are bounded by
		// the lengths we pass.
		let samples = unsafe {
			opus_multistream_decode_float(
				self.inner,
				packet.as_ptr(),
				packet.len() as i32,
				out.as_mut_ptr(),
				self.max_frame_size as i32,
				0,
			)
		};
		if samples < 0 {
			return Err(opus::decode_error(samples));
		}
		out.truncate(samples as usize * channels);

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
		Ok(Decoded { samples: out, activity })
	}

	fn reset(&mut self) -> Result<(), Error> {
		// SAFETY: `inner` owns a live decoder and OPUS_RESET_STATE takes no arguments.
		let rc = unsafe { opus_multistream_decoder_ctl_impl(self.inner, OPUS_RESET_STATE, varargs![]) };
		if rc != OPUS_OK {
			return Err(opus::error(rc, "OPUS_RESET_STATE"));
		}
		self.in_dtx = false;
		Ok(())
	}

	fn sample_rate(&self) -> u32 {
		self.sample_rate
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

impl Drop for Libopus {
	fn drop(&mut self) {
		// SAFETY: `inner` is a live OpusMSDecoder that nothing else aliases.
		unsafe { opus_multistream_decoder_destroy(self.inner) };
	}
}

#[cfg(test)]
mod tests {
	use unsafe_libopus::{
		OPUS_APPLICATION_AUDIO, opus_multistream_encode_float, opus_multistream_encoder_destroy,
		opus_multistream_surround_encoder_create,
	};

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

	/// Encode `channels` of Vorbis-ordered tones as a family 1 stream, returning
	/// its OpusHead and packets.
	fn surround(channels: usize) -> (bytes::Bytes, Vec<Vec<u8>>) {
		let speakers = VORBIS[channels - 1];
		let (mut streams, mut coupled, mut mapping) = (0i32, 0i32, [0u8; 8]);
		let mut err = 0i32;
		// SAFETY: every out-pointer is valid and `mapping` holds `channels` entries.
		let encoder = unsafe {
			opus_multistream_surround_encoder_create(
				RATE as i32,
				channels as i32,
				1,
				&mut streams,
				&mut coupled,
				mapping.as_mut_ptr(),
				OPUS_APPLICATION_AUDIO,
				&mut err,
			)
		};
		assert!(err == 0 && !encoder.is_null(), "encoder create failed: {err}");

		let packets = (0..PACKETS)
			.map(|packet| {
				let mut pcm = Vec::with_capacity(FRAME * channels);
				for i in packet * FRAME..(packet + 1) * FRAME {
					for &speaker in speakers {
						let phase = std::f32::consts::TAU * tone(speaker) * i as f32 / RATE as f32;
						pcm.push(phase.sin() * 0.5);
					}
				}
				let mut out = vec![0u8; 4000];
				// SAFETY: `encoder` is live, `pcm` holds FRAME frames, and `out` is as long as we say.
				let len = unsafe {
					opus_multistream_encode_float(encoder, pcm.as_ptr(), FRAME as i32, out.as_mut_ptr(), 4000)
				};
				assert!(len > 0, "encode failed: {len}");
				out.truncate(len as usize);
				out
			})
			.collect();
		// SAFETY: `encoder` is live and not used again.
		unsafe { opus_multistream_encoder_destroy(encoder) };

		let mut head = moq_mux::codec::opus::Config::new(RATE as u32, 2)
			.encode()
			.unwrap()
			.to_vec();
		head[9] = channels as u8;
		head[18] = 1;
		head.extend_from_slice(&[streams as u8, coupled as u8]);
		head.extend_from_slice(&mapping[..channels]);
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
				pcm.extend(decoder.decode(packet).unwrap().samples);
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
		let mid = decoder.decode(&packets[PACKETS / 2]).unwrap();
		assert!(mid.activity.is_active(), "a coded surround frame must stay active");

		// Two coupled streams, then two mono, each a 20 ms empty frame.
		let dtx = [0xfc, 0x00, 0xfc, 0x00, 0xf8, 0x00, 0xf8];
		let mut decoder = Decoder::new(&catalog(head, 6), &Config::default()).unwrap();
		let decoded = decoder.decode(&dtx).expect("empty multistream packet");
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

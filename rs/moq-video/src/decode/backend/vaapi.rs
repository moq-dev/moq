//! Intel/AMD VAAPI hardware H.264 and H.265 decode via the `moq-vaapi` crate on Linux.
//!
//! The decode half of [`encode::backend::vaapi`](crate::encode). `moq-vaapi` is a
//! focused VA-API H.264 and H.265 codec vendored and trimmed from cros-libva +
//! discord/cros-codecs. Its decoder takes one Annex-B access unit with the
//! parameter sets inline and hands back tightly-packed NV12, which this
//! deinterleaves to the CPU I420 the rest of the crate speaks.
//!
//! libva is `dlopen`'d at runtime, so a VAAPI-enabled build needs no libva at
//! build time and the binary carries no `NEEDED libva`. A libva-less host, a
//! missing render node, or a driver with no decode entrypoint for the codec
//! makes `Decoder::new` return an error; under automatic selection
//! [`backend::open`](super::open) then moves on to the next candidate, like the
//! NVDEC backend. When every render node's driver answers that it has no decode
//! entrypoint for the profile the catalog declares, or for H.265 Main at all,
//! that is a refusal of the stream rather than a fault. For H.265, which has no
//! software decoder behind this one, that refusal is what lets a player pick
//! another rendition. The render node is the one the encoder and the GPU resize
//! share, unless that one cannot decode H.265, and `Config::vaapi` can name
//! it; see `frame::vaapi::device`.
//!
//! Progressive 8-bit 4:2:0 only, which is everything a browser's `VideoEncoder`,
//! WebRTC, or this crate's own encoders emit; H.265 is the Main profile. The
//! decoder rejects an interlaced or high-bit-depth sequence at its first SPS,
//! which surfaces here as a decode error rather than wrong pixels. Left and top
//! cropping are also rejected.
//!
//! Output trails the input by the reorder depth the stream declares and no
//! more: `moq-vaapi` releases a picture as soon as the SPS's reorder limit
//! allows, so a stream without B-frames, which is what this crate's encoders,
//! WebRTC, and openh264 emit, comes out as it decodes. A stream with B-frames
//! keeps pictures in hand, which is why this backend implements
//! [`Backend::flush`] and the layers above call it when the track ends. The
//! decoded frames carry their own timestamps, so the delay itself needs nothing
//! downstream.
//!
//! Under [`Output::Native`](crate::Output::Native) each picture comes back as
//! the zero-copy [`Surface::DmaBuf`] the hardware decoded it into. Measured on
//! Intel Meteor Lake with iHD, a decode target exports at modifier
//! `0x100000000000009`, and the renderer imports that a memory plane at a time,
//! so the pixels reach a texture untouched
//! (`decoded_frames_reach_the_gpu_without_a_download` in the render module
//! draws them). A driver that decodes but cannot export falls back to
//! downloading, which native output permits.
//!
//! [`Output::Cpu`](crate::Output::Cpu) downloads inside the backend rather than
//! leaving it to the generic conversion, because exporting is not free: each
//! picture costs a PRIME export, and its surface stays out of the decoder's
//! recycling pool until the frame drops, since a later picture decoded over it
//! would corrupt one the consumer still holds. A native consumer that draws a
//! picture and lets it go gets the surface decoded into again rather than a new
//! allocation per picture. One that still wants pixels is not stranded:
//! [`Surface::into_i420`](crate::Surface::into_i420) answers, because
//! `moq-vaapi` keeps the surface alongside the descriptor and reads it back
//! through `vaDeriveImage` rather than trying to read a tiled buffer as rows.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use moq_net::Timestamp;
use moq_vaapi::decode::{Config as VaapiConfig, Decoder, ExportedFrame};
use moq_vaapi::{Display, DrmDeviceIterator, VAEntrypoint, VAProfile};

use super::{Backend, Codec, Config, Stream};
use crate::frame::{I420, Surface, vaapi};
use crate::{Error, Frame, Output};

pub(crate) const NAME: &str = "vaapi";

pub(crate) struct Vaapi {
	decoder: Decoder,
	export: Export,
	ready: VecDeque<Frame>,
}

/// Whether pictures are handed out as DMA-BUFs rather than downloaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Export {
	/// [`Config::output`] asked for shared pictures, and none has come back yet,
	/// so a failure is read as the driver being unable to export. See
	/// [`Vaapi::exported`].
	Trying,
	/// A picture has come back as a descriptor, which settles it.
	Exporting,
	/// Downloaded: asked for, or the driver turned out not to export, so a host
	/// that decodes but cannot share what it decoded loses the fast path rather
	/// than the stream.
	Downloading,
}

impl Vaapi {
	/// VA-API AV1 decode exists but is not wired up in `moq-vaapi`, so this
	/// handles H.264 and H.265. `config` carries no hardware scaler request we can
	/// honor: VA-API's scaler is a separate VPP pipeline, so callers scale the
	/// frames themselves.
	pub(crate) fn open(stream: &Stream, config: &Config) -> Result<Box<dyn Backend>, Error> {
		let (codec, required) = match stream.codec {
			Codec::H264 => {
				let required = match stream.h264_profile {
					// `moq-vaapi` fails a profile it has no mapping for at the first
					// SPS, whatever the driver offers, so a declared one is refused
					// here.
					Some(profile_idc) => match va_profile(profile_idc) {
						Some(profile) => Some((profile, format!("H.264 profile_idc {profile_idc}"))),
						None => {
							return Err(Error::UnsupportedCodec(format!(
								"VA-API decodes no H.264 profile_idc {profile_idc}"
							)));
						}
					},
					None => None,
				};
				(moq_vaapi::Codec::H264, required)
			}
			Codec::H265 => (
				moq_vaapi::Codec::H265,
				Some((VAProfile::VAProfileHEVCMain, "H.265 Main".to_owned())),
			),
			other => return Err(Error::codec(anyhow::anyhow!("VAAPI cannot decode {}", other.label()))),
		};
		if let Some((profile, name)) = required
			&& offered(codec, config.vaapi.as_deref(), profile) == Some(false)
		{
			return Err(Error::UnsupportedCodec(format!("no VA-API driver here decodes {name}")));
		}

		let vaapi = VaapiConfig {
			codec,
			device: vaapi::device(codec, config.vaapi.as_deref()),
		};
		let decoder = Decoder::new(vaapi).map_err(|e| Error::codec(anyhow::anyhow!("VAAPI decoder init: {e:?}")))?;

		// The renderer imports the NV12 this exports, so `Render` keeps it too
		// in a build that has the renderer; without one it would only download.
		let exporting = match config.output {
			Output::Native => true,
			Output::Cpu => false,
			Output::Render => cfg!(feature = "render"),
		};
		let export = match exporting {
			true => Export::Trying,
			false => Export::Downloading,
		};
		tracing::info!(
			decoder = NAME,
			codec = stream.codec.label(),
			exporting,
			"opened decoder"
		);
		Ok(Box::new(Self {
			decoder,
			export,
			ready: VecDeque::new(),
		}))
	}

	/// Decodes one access unit into GPU-resident frames, or `None` once the
	/// driver has shown it will not export.
	fn decode_shared(&mut self, access_unit: &Bytes, timestamp: u64) -> Option<Result<(), Error>> {
		if self.export == Export::Downloading {
			return None;
		}

		let exported = self.decoder.decode_exported(access_unit, timestamp).and_then(share);
		Some(self.exported(exported))
	}

	/// The same for the stream's tail, so a track that ends does not switch to
	/// downloading for its last few pictures.
	fn flush_shared(&mut self) -> Option<Result<(), Error>> {
		if self.export == Export::Downloading {
			return None;
		}

		let exported = self.decoder.flush_exported().and_then(share);
		Some(self.exported(exported))
	}

	/// Queues the pictures a shared decode produced, and decides what a failure
	/// to produce any means.
	///
	/// A driver can decode without being able to share what it decoded, and
	/// native output permits CPU pictures, so until one picture
	/// has come back that way a failure is read as this driver answering the
	/// question. Losing the pictures of the call that found out beats losing the
	/// stream, and the DPB is untouched by it: those pictures had already been
	/// bumped out of it.
	///
	/// Once a picture has come back the question is settled, and a later failure
	/// is a decode error rather than a verdict on the driver. Reporting it as
	/// one keeps a corrupt access unit from silently costing the rest of the
	/// session its fast path.
	fn exported(&mut self, exported: anyhow::Result<Vec<Frame>>) -> Result<(), Error> {
		match exported {
			Ok(frames) => {
				// An access unit whose pictures are all still in the DPB proves
				// nothing about exporting, so only a non-empty answer counts.
				if !frames.is_empty() {
					self.export = Export::Exporting;
				}
				self.ready.extend(frames);
				Ok(())
			}
			Err(err) if self.export == Export::Exporting => {
				Err(Error::codec(err.context("VAAPI decode to a shared surface")))
			}
			Err(err) => {
				tracing::warn!(%err, "VAAPI cannot hand out decoded surfaces; downloading them instead");
				self.export = Export::Downloading;
				Ok(())
			}
		}
	}
}

impl Backend for Vaapi {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<(), Error> {
		// The timestamp rides through the decoder with the picture, so it
		// survives the DPB reordering a stream with B-frames goes through.
		let timestamp = timestamp.as_micros() as u64;
		if let Some(frames) = self.decode_shared(&access_unit, timestamp) {
			return frames;
		}

		let decoded = self
			.decoder
			.decode(&access_unit, timestamp)
			.map_err(|e| Error::codec(anyhow::anyhow!("VAAPI decode: {e:?}")))?;

		self.convert(decoded)
	}

	fn receive(&mut self) -> Result<Option<Frame>, Error> {
		Ok(self.ready.pop_front())
	}

	/// Queue the tail held by the DPB at the end of the stream.
	fn flush(&mut self) -> Result<(), Error> {
		if let Some(frames) = self.flush_shared() {
			return frames;
		}

		let decoded = self
			.decoder
			.flush()
			.map_err(|e| Error::codec(anyhow::anyhow!("VAAPI flush: {e:?}")))?;

		self.convert(decoded)
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// The VA profile an H.264 `profile_idc` decodes under, the mapping `moq-vaapi`
/// opens its config with; `None` for a profile it can't decode, such as High
/// 4:4:4 Predictive (244), which its SPS parser rejects.
fn va_profile(profile_idc: u8) -> Option<VAProfile::Type> {
	match profile_idc {
		66 => Some(VAProfile::VAProfileH264ConstrainedBaseline),
		77 | 88 => Some(VAProfile::VAProfileH264Main),
		100 | 110 | 122 => Some(VAProfile::VAProfileH264High),
		_ => None,
	}
}

/// Whether a render node a `codec` decoder could open offers VLD decode for
/// `profile`.
///
/// `Some(false)` only when every node answered and none does. A node that can't
/// be asked, such as one whose VA driver is missing, leaves the answer unknown,
/// since a missing driver is a fault rather than the device's refusal.
fn offered(codec: moq_vaapi::Codec, named: Option<&Path>, profile: VAProfile::Type) -> Option<bool> {
	let nodes: Vec<PathBuf> = match vaapi::device(codec, named) {
		Some(node) => vec![node],
		None => DrmDeviceIterator::default().collect(),
	};
	if nodes.is_empty() {
		return None;
	}
	for node in nodes {
		let display = Display::open_drm_display(&node).ok()?;
		if display.query_config_profiles().ok()?.contains(&profile)
			&& display
				.query_config_entrypoints(profile)
				.ok()?
				.contains(&VAEntrypoint::VAEntrypointVLD)
		{
			return Some(true);
		}
	}
	Some(false)
}

impl Vaapi {
	/// Deinterleave each decoded picture's NV12 into the CPU I420 the rest of the
	/// crate speaks, keeping the timestamp the picture was coded with.
	fn convert(&mut self, decoded: Vec<moq_vaapi::decode::Frame>) -> Result<(), Error> {
		for frame in decoded {
			let i420 = I420::from_nv12(&frame.data, crate::Size::new(frame.width, frame.height))?;
			let timestamp = Timestamp::from_micros(frame.timestamp).unwrap_or(Timestamp::ZERO);
			self.ready.push_back(Frame::new(Surface::I420(i420), timestamp));
		}
		Ok(())
	}
}

/// Wrap each exported picture as the DMA-BUF surface a renderer imports, keeping
/// the timestamp the picture was coded with.
fn share(exported: Vec<ExportedFrame>) -> anyhow::Result<Vec<Frame>> {
	exported
		.into_iter()
		.map(|frame| {
			let timestamp = Timestamp::from_micros(frame.timestamp).unwrap_or(Timestamp::ZERO);
			// A decode target names no color space, so the renderer infers one
			// from the frame size exactly as it does for a downloaded picture.
			Ok(Frame::new(Surface::DmaBuf(vaapi::adopt(frame, None)?), timestamp))
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::decode::{Config as DecodeConfig, Kind as DecodeKind};
	use crate::encode::{Config as EncodeConfig, Encoder, Kind as EncodeKind};

	/// Real hardware only: skip on a box with no libva or no VA-API H.264 decode
	/// entrypoint, so these are no-ops on CI and validate on an Intel or AMD box.
	fn hw_available() -> bool {
		Decoder::new(VaapiConfig::new()).is_ok()
	}

	fn decode_config() -> DecodeConfig {
		DecodeConfig {
			kind: DecodeKind::Named(NAME.into()),
			output: Output::Cpu,
			..DecodeConfig::new()
		}
	}

	fn gpu_decode_config() -> DecodeConfig {
		DecodeConfig {
			output: Output::Native,
			..decode_config()
		}
	}

	/// On a host with no usable VA stack, opening must return an `Err` (so
	/// `Kind::Auto` falls through to openh264) rather than panicking inside libva.
	/// A no-op on a box that does have one.
	#[test]
	fn missing_driver_errors_instead_of_panicking() {
		if hw_available() {
			return;
		}
		assert!(Vaapi::open(&Codec::H264.into(), &decode_config()).is_err());
	}

	/// A declared profile `moq-vaapi` can't decode, such as High 4:4:4 Predictive,
	/// is refused before any driver is asked, on any host.
	#[test]
	fn an_unmapped_profile_is_refused() {
		let stream = Stream {
			h264_profile: Some(244),
			..Codec::H264.into()
		};
		match Vaapi::open(&stream, &decode_config()) {
			Err(Error::UnsupportedCodec(reason)) => assert!(reason.contains("244"), "{reason}"),
			Err(other) => panic!("expected a refusal, got {other:?}"),
			Ok(_) => panic!("opened a High 4:4:4 Predictive decoder"),
		}
	}

	/// A static RGBA gradient that varies in both axes, so the chroma planes have
	/// spatial structure and a pitch or plane-split bug corrupts the picture.
	fn gradient_rgba(width: u32, height: u32) -> Vec<u8> {
		let (w, h) = (width as usize, height as usize);
		let mut buf = vec![0u8; w * h * 4];
		for y in 0..h {
			for x in 0..w {
				let i = (y * w + x) * 4;
				buf[i] = (x * 255 / w) as u8;
				buf[i + 1] = (y * 255 / h) as u8;
				buf[i + 2] = ((x + y) * 255 / (w + h)) as u8;
				buf[i + 3] = 255;
			}
		}
		buf
	}

	/// Mean absolute error between two equal-length planes.
	fn mae(a: &[u8], b: &[u8]) -> u64 {
		assert_eq!(a.len(), b.len());
		a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() / a.len() as u64
	}

	/// H.264 through the real hardware: openh264 encodes a gradient (Annex-B with
	/// inline SPS/PPS) and VA-API decodes it. Asserts the downloaded picture
	/// matches the input, which a plane split or stride bug would shear, and that
	/// the timestamp rides through with its picture.
	#[test]
	fn vaapi_h264_round_trip() {
		if !hw_available() {
			return;
		}
		let (w, h) = (320u32, 240u32);
		let rgba = gradient_rgba(w, h);
		let expected = I420::from_rgba(&rgba, w * 4, crate::Size::new(w, h)).unwrap();

		let mut encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, crate::FrameRate::new(30, 1).unwrap())
		})
		.unwrap();
		let mut decoder = Vaapi::open(&Codec::H264.into(), &decode_config()).expect("VAAPI H.264 decoder");

		let mut decoded = Vec::new();
		for i in 0..10u64 {
			if i == 0 {
				encoder.cut().unwrap();
			}
			let surface = Surface::rgba(&rgba, crate::Size::new(w, h)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(i * 33_333).unwrap());
			for encoded in encoder.encode_all(&frame).unwrap() {
				decoded.extend(decoder.decode_all(encoded.payload, encoded.timestamp, i == 0).unwrap());
			}
		}

		assert!(!decoded.is_empty(), "VAAPI produced no frames");
		for (i, frame) in decoded.iter().enumerate() {
			// openh264 encodes IPPP, so the pictures come back in feed order.
			assert_eq!(
				frame.timestamp.as_micros(),
				i as u128 * 33_333,
				"timestamp did not ride the picture"
			);
			let i420 = frame.surface.to_i420().unwrap();
			assert_eq!((i420.width(), i420.height()), (w, h));
			assert!(mae(i420.y(), expected.y()) < 8, "Y plane corrupt");
			assert!(mae(i420.u(), expected.u()) < 8, "U plane corrupt");
			assert!(mae(i420.v(), expected.v()) < 8, "V plane corrupt");
		}
	}

	/// Native output hands out DMA-BUFs, and a CPU consumer of one gets the same
	/// pixels it would have got by asking for CPU output.
	///
	/// The bargain the default rests on: native output does not take
	/// `Surface::into_i420` away from whatever the frames are handed to. Byte-exact rather than approximate, because both sides are the same
	/// hardware decoding the same units, and the read-back goes through the same
	/// `vaDeriveImage` path the download does.
	///
	/// Needs no GPU beyond the VA-API device: this is about what a picture on the
	/// GPU can still do for a consumer that is not on one.
	#[test]
	fn native_frames_still_answer_into_i420() {
		if !hw_available() {
			return;
		}
		let (w, h) = (320u32, 240u32);
		let rgba = gradient_rgba(w, h);

		let mut encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, crate::FrameRate::new(30, 1).unwrap())
		})
		.unwrap();
		let mut exporting = Vaapi::open(&Codec::H264.into(), &gpu_decode_config()).expect("VAAPI H.264 decoder");
		let mut downloading = Vaapi::open(&Codec::H264.into(), &decode_config()).expect("a second decoder");

		let mut exported = Vec::new();
		let mut downloaded = Vec::new();
		for i in 0..10u64 {
			if i == 0 {
				encoder.cut().unwrap();
			}
			let surface = Surface::rgba(&rgba, crate::Size::new(w, h)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(i * 33_333).unwrap());
			for encoded in encoder.encode_all(&frame).unwrap() {
				let (payload, timestamp) = (encoded.payload, encoded.timestamp);
				exported.extend(exporting.decode_all(payload.clone(), timestamp, i == 0).unwrap());
				downloaded.extend(downloading.decode_all(payload, timestamp, i == 0).unwrap());
			}
		}
		assert!(!exported.is_empty(), "VAAPI produced no frames");
		assert_eq!(exported.len(), downloaded.len(), "the two decoders disagreed");

		for (i, (gpu, cpu)) in exported.iter().zip(&downloaded).enumerate() {
			let Surface::DmaBuf(buffer) = &gpu.surface else {
				panic!("frame {i} did not come back GPU-resident");
			};
			assert_eq!(buffer.format(), crate::DrmFormat::NV12);
			assert_eq!((buffer.width(), buffer.height()), (w, h));
			assert_eq!(gpu.timestamp, cpu.timestamp, "frame {i} lost its timestamp");

			let Surface::I420(reference) = &cpu.surface else {
				panic!("frame {i} came back GPU-resident under CPU output");
			};
			let read_back = gpu.surface.to_i420().expect("read the decoded surface back");
			assert_eq!(
				(read_back.width(), read_back.height()),
				(reference.width(), reference.height())
			);
			assert!(
				read_back.y() == reference.y(),
				"frame {i} read back a different Y plane"
			);
			assert!(
				read_back.u() == reference.u(),
				"frame {i} read back a different U plane"
			);
			assert!(
				read_back.v() == reference.v(),
				"frame {i} read back a different V plane"
			);
		}
	}

	/// Regression: every picture fed in comes back out, which needs the flush.
	///
	/// A stream with B-frames keeps pictures in the DPB until later ones come
	/// out ahead of them, so a stream simply stopping leaves its tail there. The
	/// fixture is libx264 at High profile with three B-frames. The `decode` half
	/// of the assertion is what keeps this honest, since a decoder that held
	/// nothing back would pass the rest of it without a flush ever running.
	#[test]
	fn flushing_returns_the_tail_the_dpb_holds() {
		if !hw_available() {
			return;
		}
		flushing_returns_the_tail(&decode_config());
	}

	/// The same for GPU-resident output, which drains through `flush_exported`
	/// rather than `flush`. Without its own case it would be the one path where a
	/// track's last pictures go missing, since the two drains are separate calls
	/// into `moq-vaapi` and only one of them is on the default path.
	#[test]
	fn flushing_returns_the_tail_of_a_gpu_stream() {
		if !hw_available() {
			return;
		}
		flushing_returns_the_tail(&gpu_decode_config());
	}

	fn flushing_returns_the_tail(config: &DecodeConfig) {
		let units = crate::decode::conformance::split(include_bytes!("../test_data/bframes_64x64_pattern_30f.h264"));
		let mut decoder = Vaapi::open(&Codec::H264.into(), config).expect("VAAPI H.264 decoder");

		let mut streamed = Vec::new();
		for unit in &units {
			streamed.extend(
				decoder
					.decode_all(unit.payload.clone(), unit.timestamp, unit.keyframe)
					.unwrap(),
			);
		}
		assert!(
			streamed.len() < units.len(),
			"the DPB held nothing back, so this test proves nothing"
		);

		let flushed = decoder.flush_all().unwrap();
		let mut timestamps: Vec<Timestamp> = streamed.iter().chain(&flushed).map(|frame| frame.timestamp).collect();
		timestamps.sort_unstable();
		let expected: Vec<Timestamp> = units.iter().map(|unit| unit.timestamp).collect();
		assert_eq!(timestamps, expected, "the stream lost pictures at its end");

		// A second flush has nothing left to hand back, so the drain is idempotent
		// and a caller that flushes twice does not see a picture twice.
		assert!(decoder.flush_all().unwrap().is_empty());
	}

	/// A stream without B-frames comes out as it decodes, one picture per
	/// access unit with nothing left for the flush.
	///
	/// openh264 declares one frame buffer, so a decoder that waits for the next
	/// picture to need it hands each picture out one access unit late. A player
	/// that keeps only the newest picture would show every one a frame interval
	/// behind.
	#[test]
	fn a_stream_without_b_frames_is_not_held_back() {
		if !hw_available() {
			return;
		}
		for config in [decode_config(), gpu_decode_config()] {
			let (w, h) = (320u32, 240u32);
			let rgba = gradient_rgba(w, h);
			let mut encoder = Encoder::new(&EncodeConfig {
				kind: EncodeKind::Software,
				..EncodeConfig::new(w, h, crate::FrameRate::new(30, 1).unwrap())
			})
			.unwrap();
			let mut decoder = Vaapi::open(&Codec::H264.into(), &config).expect("VAAPI H.264 decoder");

			for i in 0..5u64 {
				if i == 0 {
					encoder.cut().unwrap();
				}
				let surface = Surface::rgba(&rgba, crate::Size::new(w, h)).unwrap();
				let timestamp = Timestamp::from_micros(i * 33_333).unwrap();
				for encoded in encoder.encode_all(&Frame::new(surface, timestamp)).unwrap() {
					let decoded = decoder.decode_all(encoded.payload, encoded.timestamp, i == 0).unwrap();
					let timestamps: Vec<Timestamp> = decoded.iter().map(|frame| frame.timestamp).collect();
					assert_eq!(timestamps, [timestamp], "picture {i} was held back");
				}
			}
			assert!(decoder.flush_all().unwrap().is_empty());
		}
	}
}

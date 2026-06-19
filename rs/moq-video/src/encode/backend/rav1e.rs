//! Software AV1 backend via rav1e (pure Rust, statically linked).
//!
//! The fallback for AV1 when no hardware AV1 encoder is wired up (which is all
//! platforms today). Emits a low-overhead OBU bitstream with an inline sequence
//! header ahead of each keyframe, ready for `moq_mux::codec::av1::Split` +
//! `Import`. Built `default-features = false` so it needs no nasm at build time;
//! correctness over speed for the software path.

use bytes::Bytes;
use rav1e::prelude::*;

use super::Backend;
use crate::Error;
use crate::frame::Frame;

pub(crate) const NAME: &str = "rav1e";

pub(crate) struct Rav1e {
	ctx: Context<u8>,
}

impl Rav1e {
	pub(crate) fn open(config: &super::super::encoder::Config) -> Result<Box<dyn Backend>, Error> {
		let mut enc = EncoderConfig {
			width: config.width as usize,
			height: config.height as usize,
			bit_depth: 8,
			chroma_sampling: ChromaSampling::Cs420,
			// Real-time camera: keep the encoder one-in-one-out (no lookahead
			// buffering) so each captured frame yields its packet immediately and
			// the publisher's per-frame timestamp lines up.
			low_latency: true,
			min_key_frame_interval: 0,
			max_key_frame_interval: config.gop as u64,
			bitrate: clamp_i32(config.resolved_bitrate()),
			// Fastest preset: software AV1 is the fallback, prioritize latency.
			speed_settings: SpeedSettings::from_preset(10),
			..Default::default()
		};
		enc.speed_settings.rdo_lookahead_frames = 1;

		let cfg = rav1e::Config::new().with_encoder_config(enc);
		let ctx: Context<u8> = cfg
			.new_context()
			.map_err(|e| Error::Codec(anyhow::anyhow!("rav1e init: {e}")))?;

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened AV1 encoder"
		);
		Ok(Box::new(Self { ctx }))
	}
}

impl Backend for Rav1e {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		// Software path: needs CPU I420, downloading a GPU surface if necessary.
		let i420 = frame.to_i420()?;
		let w = i420.width as usize;

		let mut f = self.ctx.new_frame();
		// Planes are tightly packed I420: Y is full width, U/V are half. The row
		// count comes from the encoder's configured dimensions (the Encoder front
		// end guarantees the frame matches), so only the source stride matters here.
		f.planes[0].copy_from_raw_u8(i420.y(), w, 1);
		f.planes[1].copy_from_raw_u8(i420.u(), w / 2, 1);
		f.planes[2].copy_from_raw_u8(i420.v(), w / 2, 1);

		let params = FrameParameters {
			frame_type_override: if keyframe {
				FrameTypeOverride::Key
			} else {
				FrameTypeOverride::No
			},
			opaque: None,
			t35_metadata: Box::new([]),
		};

		self.ctx
			.send_frame((f, Some(params)))
			.map_err(|e| Error::Codec(anyhow::anyhow!("rav1e send_frame: {e}")))?;

		drain(&mut self.ctx)
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		self.ctx.flush();
		drain(&mut self.ctx)
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// Pull every packet rav1e can currently emit. `NeedMoreData` / `LimitReached`
/// end the drain; `Encoded` means a frame went in without a packet out yet, so
/// keep polling.
fn drain(ctx: &mut Context<u8>) -> Result<Vec<Bytes>, Error> {
	let mut out = Vec::new();
	loop {
		match ctx.receive_packet() {
			Ok(packet) => out.push(Bytes::from(packet.data)),
			Err(EncoderStatus::Encoded) => continue,
			Err(EncoderStatus::NeedMoreData | EncoderStatus::LimitReached) => break,
			Err(e) => return Err(Error::Codec(anyhow::anyhow!("rav1e receive_packet: {e}"))),
		}
	}
	Ok(out)
}

fn clamp_i32(value: u64) -> i32 {
	value.min(i32::MAX as u64) as i32
}

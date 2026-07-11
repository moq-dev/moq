//! Per-broadcast [`MediaSink`](crate::session::MediaSink) used by every
//! RTP-in flow (`server publish` / WHIP server, `client subscribe` / WHEP
//! client).
//!
//! Holds the [`moq_net::broadcast::Producer`] and per-track codec bridges.
//! On `MediaAdded`, it inspects the negotiated codec and instantiates the
//! matching bridge; on each `MediaData`, it forwards into the bridge.

use crate::{Error, Result, codec, session};

pub struct IngestSink {
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	bridges: session::Bridges,
}

impl IngestSink {
	pub fn new(mut broadcast: moq_net::broadcast::Producer) -> Result<Self> {
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		Ok(Self {
			broadcast,
			catalog,
			bridges: session::Bridges::new(),
		})
	}
}

impl session::MediaSink for IngestSink {
	fn on_track(
		&mut self,
		mid: str0m::media::Mid,
		_kind: str0m::media::MediaKind,
		codec_kind: str0m::format::Codec,
		audio_params: Option<(u32, u32)>,
	) -> Result<()> {
		let bridge: Box<dyn codec::Bridge> = match codec_kind {
			str0m::format::Codec::Opus => {
				let (sample_rate, channels) = audio_params.unwrap_or((48_000, 2));
				Box::new(codec::opus::Bridge::new(
					self.broadcast.clone(),
					self.catalog.clone(),
					sample_rate,
					channels,
				)?)
			}
			str0m::format::Codec::H264 => {
				Box::new(codec::h264::Bridge::new(self.broadcast.clone(), self.catalog.clone())?)
			}
			str0m::format::Codec::H265 => {
				Box::new(codec::h265::Bridge::new(self.broadcast.clone(), self.catalog.clone())?)
			}
			str0m::format::Codec::Vp8 => {
				Box::new(codec::vp8::Bridge::new(self.broadcast.clone(), self.catalog.clone())?)
			}
			str0m::format::Codec::Vp9 => {
				Box::new(codec::vp9::Bridge::new(self.broadcast.clone(), self.catalog.clone())?)
			}
			str0m::format::Codec::Av1 => {
				Box::new(codec::av1::Bridge::new(self.broadcast.clone(), self.catalog.clone())?)
			}
			other => return Err(Error::UnsupportedCodec(format!("{other:?}"))),
		};
		self.bridges.insert(mid, bridge);
		Ok(())
	}

	fn on_frame(&mut self, mid: str0m::media::Mid, frame: codec::Frame) -> Result<()> {
		self.bridges.push(mid, frame)
	}

	fn abort(&mut self, err: moq_net::Error) {
		self.bridges.abort(err);
	}
}

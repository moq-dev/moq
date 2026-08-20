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
	pub fn new(mut broadcast: moq_net::broadcast::Producer, max_age: Option<std::time::Duration>) -> Result<Self> {
		let config = moq_mux::catalog::Config::default().with_max_age(max_age);
		let catalog = moq_mux::catalog::Producer::with_config(&mut broadcast, config)?;
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

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::session::MediaSink;

	/// The retention the caller configured has to land on the media tracks the codec
	/// bridges mint, not just on the catalog producer it was set on.
	#[tokio::test]
	async fn tracks_inherit_the_configured_retention() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let mut sink = IngestSink::new(broadcast.clone(), Some(Duration::from_secs(3))).unwrap();

		sink.on_track(
			"0".into(),
			str0m::media::MediaKind::Video,
			str0m::format::Codec::H264,
			None,
		)
		.unwrap();

		let info = broadcast.consume().track("0.avc3").unwrap().info().await.unwrap();
		assert_eq!(info.max_age, Duration::from_secs(3));
	}
}

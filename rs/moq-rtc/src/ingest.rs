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
	reserved: Option<moq_mux::catalog::Reserved>,
	pending_bridges: usize,
	bridges: session::Bridges,
}

impl IngestSink {
	pub fn new(mut broadcast: moq_net::broadcast::Producer, expected_bridges: usize) -> Result<Self> {
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let reserved = (expected_bridges > 0).then(|| catalog.reserve());
		Ok(Self {
			broadcast,
			catalog,
			reserved,
			pending_bridges: expected_bridges,
			bridges: session::Bridges::new(),
		})
	}

	fn reserve(&self) -> moq_mux::catalog::Reserved {
		self.reserved
			.as_ref()
			.cloned()
			.unwrap_or_else(|| self.catalog.reserve())
	}

	fn finish_bridge(&mut self) {
		self.pending_bridges = self.pending_bridges.saturating_sub(1);
		if self.pending_bridges == 0 {
			self.reserved = None;
		}
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
					self.reserve(),
					sample_rate,
					channels,
				)?)
			}
			str0m::format::Codec::H264 => Box::new(codec::h264::Bridge::new(self.broadcast.clone(), self.reserve())?),
			str0m::format::Codec::Vp8 => Box::new(codec::vp8::Bridge::new(self.broadcast.clone(), self.reserve())?),
			str0m::format::Codec::Vp9 => Box::new(codec::vp9::Bridge::new(self.broadcast.clone(), self.reserve())?),
			other => return Err(Error::UnsupportedCodec(format!("{other:?}"))),
		};
		self.bridges.insert(mid, bridge);
		self.finish_bridge();
		Ok(())
	}

	fn on_frame(&mut self, mid: str0m::media::Mid, frame: codec::Frame) -> Result<()> {
		self.bridges.push(mid, frame)
	}
}

#[cfg(test)]
mod tests {
	use std::task::Poll;

	use crate::session::MediaSink;

	use super::*;

	#[test]
	fn shared_reservation_waits_for_all_bridges_and_lazy_video_config() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let mut sink = IngestSink::new(broadcast, 2).unwrap();
		let mut consumer = sink.catalog.consume().unwrap();
		let waiter = kio::Waiter::noop();

		sink.on_track(
			str0m::media::Mid::from("audio"),
			str0m::media::MediaKind::Audio,
			str0m::format::Codec::Opus,
			Some((48_000, 2)),
		)
		.unwrap();
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"audio must wait for the negotiated video bridge"
		);

		sink.on_track(
			str0m::media::Mid::from("video"),
			str0m::media::MediaKind::Video,
			str0m::format::Codec::Vp8,
			None,
		)
		.unwrap();
		assert!(
			matches!(consumer.poll_next(&waiter), Poll::Pending),
			"lazy VP8 config must still gate the first catalog"
		);

		sink.on_frame(
			str0m::media::Mid::from("video"),
			codec::Frame {
				timestamp_us: 0,
				payload: bytes::Bytes::from_static(&[0]),
			},
		)
		.unwrap();

		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(catalog))) => catalog,
			other => panic!("expected a complete catalog, got {other:?}"),
		};
		assert_eq!(snapshot.audio.renditions.len(), 1);
		assert_eq!(snapshot.video.renditions.len(), 1);
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));
	}
}

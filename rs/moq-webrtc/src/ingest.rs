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
	/// The route announcing the broadcast, when the sink owns it: held for the session's
	/// life, linger included, and retracted when the session drops it.
	_route: Option<moq_net::origin::Route>,
}

impl IngestSink {
	/// Fill `broadcast` and its `catalog`, whose announcement the caller owns.
	pub fn new(broadcast: moq_net::broadcast::Producer, catalog: moq_mux::catalog::Producer) -> Self {
		Self {
			broadcast,
			catalog,
			bridges: session::Bridges::new(),
			_route: None,
		}
	}

	/// Fill `route`'s broadcast under a catalog minted from `config`, keeping the route for
	/// the session's life.
	pub fn routed(route: moq_net::origin::Route, config: moq_mux::catalog::Config) -> Result<Self> {
		let mut broadcast = route.broadcast().clone();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;
		let mut sink = Self::new(broadcast, catalog);
		sink._route = Some(route);
		Ok(sink)
	}
}

/// Dropped before the session ends, as when the pull's future is cancelled at shutdown, the sink
/// aborts its tracks instead of leaving them unfinished. A finished or aborted sink has nothing left.
impl Drop for IngestSink {
	fn drop(&mut self) {
		session::MediaSink::abort(self, moq_net::Error::Cancel);
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

	fn on_discontinuity(&mut self, mid: str0m::media::Mid) -> Result<()> {
		self.bridges.discontinuity(mid)
	}

	fn on_frame(&mut self, mid: str0m::media::Mid, frame: codec::Frame) -> Result<()> {
		self.bridges.push(mid, frame)
	}

	fn abort(&mut self, err: moq_net::Error) {
		self.catalog.clone().abort(err.clone());
		self.bridges.abort(err);
	}

	fn finish(&mut self) -> Result<moq_net::broadcast::Producer> {
		self.bridges.finish()?;
		self.catalog.finish()?;
		Ok(self.broadcast.clone())
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::session::MediaSink;

	/// The max age the caller configured has to land on the media tracks the codec
	/// bridges mint, not just on the catalog producer it was set on.
	#[tokio::test]
	async fn tracks_inherit_the_configured_max_age() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = moq_mux::catalog::Config::default().with_max_age(Duration::from_secs(3));
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
		let mut sink = IngestSink::new(broadcast.clone(), catalog);

		sink.on_track(
			"0".into(),
			str0m::media::MediaKind::Video,
			str0m::format::Codec::H264,
			None,
		)
		.unwrap();

		let info = broadcast.consume().track("0.avc3").unwrap().info().await.unwrap();
		assert_eq!(info.max_age, Some(Duration::from_secs(3)));
	}

	/// A clean end finishes every track, the catalog and a video track that never got a frame
	/// included, so the held sink serves them to a late subscriber for the linger.
	#[tokio::test]
	async fn finish_keeps_every_track_for_the_linger() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut sink = IngestSink::new(broadcast, catalog);
		let audio = str0m::media::MediaKind::Audio;
		sink.on_track("0".into(), audio, str0m::format::Codec::Opus, Some((48_000, 2)))
			.unwrap();
		let video = str0m::media::MediaKind::Video;
		sink.on_track("1".into(), video, str0m::format::Codec::Vp8, None)
			.unwrap();
		let payload = bytes::Bytes::from_static(&[0xfc, 0xff, 0xfe]);
		sink.on_frame(
			"0".into(),
			codec::Frame {
				timestamp_us: 20_000,
				payload,
			},
		)
		.unwrap();

		let broadcast = sink.finish().unwrap();
		let linger = moq_mux::Linger::new([broadcast], sink, Duration::from_secs(3));
		let mut catalog = consumer.track("catalog.json").unwrap().subscribe(None).await.unwrap();
		assert!(
			catalog.recv_group().await.unwrap().is_some(),
			"the late subscriber reads the catalog"
		);
		assert!(consumer.track("0.opus").is_ok());
		drop(linger);
		assert!(consumer.is_closed());
	}

	/// A step in the session timeline breaks the track's timeline with a group of its own, which
	/// is what lets a reader's clock re-anchor instead of read the step as drift.
	#[tokio::test(start_paused = true)]
	async fn a_step_publishes_a_marker_group() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut sink = IngestSink::new(broadcast, catalog);
		let audio = str0m::media::MediaKind::Audio;
		sink.on_track("0".into(), audio, str0m::format::Codec::Opus, Some((48_000, 2)))
			.unwrap();
		let frame = |timestamp_us| codec::Frame {
			timestamp_us,
			payload: bytes::Bytes::from_static(&[0xfc, 0xff, 0xfe]),
		};
		let mut track = consumer.track("0.opus").unwrap().subscribe(None).await.unwrap();
		let mut groups = 0;
		let mut read = async |track: &mut moq_net::track::Subscriber| {
			while let Ok(Ok(Some(_))) = kio::clock::timeout(Duration::from_millis(100), track.recv_group()).await {
				groups += 1;
			}
		};
		sink.on_frame("0".into(), frame(20_000)).unwrap();
		read(&mut track).await;
		sink.on_frame("0".into(), frame(40_000)).unwrap();
		read(&mut track).await;
		sink.on_discontinuity("0".into()).unwrap();
		read(&mut track).await;
		sink.on_frame("0".into(), frame(400_000)).unwrap();
		read(&mut track).await;
		assert_eq!(groups, 4, "two frames, the marker, and the frame after it");
	}

	/// Dropping a sink before its session ends, as when shutdown cancels a pull, aborts its
	/// tracks and the catalog rather than leaving them to warn as dropped unfinished.
	#[tokio::test]
	async fn a_dropped_sink_aborts_its_tracks() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut sink = IngestSink::new(broadcast, catalog.clone());
		let audio = str0m::media::MediaKind::Audio;
		sink.on_track("0".into(), audio, str0m::format::Codec::Opus, Some((48_000, 2)))
			.unwrap();
		let mut track = consumer.track("0.opus").unwrap().subscribe(None).await.unwrap();

		drop(sink);
		drop(catalog);
		assert!(matches!(track.recv_group().await, Err(moq_net::Error::Cancel)));
	}
}

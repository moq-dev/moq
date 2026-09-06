//! Subscribe to an encoded H.264, H.265, or AV1 track and emit raw I420 frames.

use std::collections::VecDeque;

use hang::catalog::VideoConfig;

use super::decoder::Config;
use super::sink::Sink;
use crate::Error;
use crate::Frame;

/// Subscribe to a moq-mux video track and emit decoded I420.
///
/// The codec/backend are fixed at construction; [`read`](Self::read) returns
/// plain [`Frame`]s. The direct mirror of `moq_audio::decode::Consumer`.
pub struct Consumer {
	/// A [`Sink`] rather than a bare `Decoder`: the read loop below is held
	/// across `.await` by every caller (libmoq's spawned task, moq-transcode),
	/// so the codec would otherwise migrate between executor workers and
	/// unbalance the per-thread COM apartment the Windows backend opens.
	decoder: Sink,
	track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	/// Frames a single access unit decoded to but `read` hasn't returned yet.
	/// One AU yields one frame in the low-delay path, but a backend may hand back
	/// more, so we buffer to keep `read` one-frame-per-call.
	pending: VecDeque<Frame>,
	/// Whether the ended track's decoder has already been drained.
	drained: bool,
	/// Last container discontinuity observed. A change starts a fresh codec epoch.
	discontinuity: u64,
}

impl Consumer {
	/// Subscribe to `name` in `broadcast`, decoding it per the catalog entry.
	/// Errors if the rendition's codec is not supported by a native backend.
	pub async fn new(
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &VideoConfig,
		name: impl Into<String>,
		config: Config,
	) -> Result<Self, Error> {
		let decoder = Sink::open(catalog, &config).await?;

		let name = name.into();
		let track = broadcast
			.track(&name)?
			.subscribe(moq_net::track::Subscription::default().with_priority(hang::catalog::PRIORITY.video))
			.await?;
		// The catalog says how the track is framed, and it is not always the legacy
		// wire: `moq import fmp4` publishes CMAF. Reading a moof+mdat fragment as a
		// varint timestamp plus a payload decodes to garbage rather than failing.
		let container = moq_mux::catalog::hang::Container::try_from(&catalog.container)?;
		let mut track = moq_mux::container::Consumer::new(track, container);
		if let Some(latency) = config.latency_max {
			track = track.with_latency(latency);
		}

		Ok(Self {
			decoder,
			track,
			pending: VecDeque::new(),
			drained: false,
			discontinuity: 0,
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"` or `"openh264"`.
	pub fn name(&self) -> &str {
		self.decoder.name()
	}

	/// Read the next decoded I420 frame, or `None` after the track ends and the
	/// decoder's buffered tail has been drained.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		loop {
			if let Some(frame) = self.pending.pop_front() {
				return Ok(Some(frame));
			}
			if self.drained {
				return Ok(None);
			}

			let mux_frame = self.track.read().await?;
			let discontinuity = self.track.discontinuity();
			if discontinuity != self.discontinuity {
				// The tail belongs to the abandoned codec epoch. Draining resets the
				// backend for reuse, but none of those pictures may cross the seam.
				self.decoder.flush().await?;
				self.pending.clear();
				self.discontinuity = discontinuity;
			}

			let Some(mux_frame) = mux_frame else {
				let tail = self.decoder.flush().await?;
				self.pending.extend(tail);
				self.drained = true;
				continue;
			};

			self.pending.extend(
				self.decoder
					.decode(mux_frame.payload, mux_frame.timestamp, mux_frame.keyframe)
					.await?,
			);
		}
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;
	use moq_net::Timestamp;

	use super::*;
	use crate::decode::Kind;
	use crate::decode::backend::probe;
	use crate::encode::{Config as EncodeConfig, Encoder, Kind as EncodeKind, Producer as EncodeProducer};

	#[tokio::test]
	async fn reads_cmaf_container_declared_by_catalog() {
		let mut source_broadcast = moq_net::broadcast::Info::new().produce();
		let source_subscriber = source_broadcast.consume();
		let source_catalog = moq_mux::catalog::Producer::new(&mut source_broadcast).unwrap();
		let config = EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(320, 240, 30)
		};
		let rendition = config.probe().await.unwrap();
		let mut producer = EncodeProducer::new(source_broadcast, source_catalog, rendition).unwrap();
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];
		for index in 0..2 {
			encoder.keyframe();
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = crate::Frame::new(surface, moq_net::Timestamp::from_micros(index * 33_333).unwrap());
			producer.publish(&encoder.encode(&frame).unwrap()).unwrap();
		}

		let origin = moq_net::Origin::random().produce();
		let mut requests = origin.dynamic();
		let served = source_subscriber.clone();
		tokio::spawn(async move {
			while let Ok(request) = requests.requested_broadcast().await {
				request.accept(served.clone());
			}
		});
		let catalog = moq_mux::catalog::Consumer::<()>::new(&source_subscriber, moq_mux::catalog::CatalogFormat::Hang)
			.await
			.unwrap();
		let source = moq_mux::Source::new(origin.consume(), "test");
		let mut export = moq_mux::container::fmp4::Export::new(source, catalog);
		let init = export.next().await.unwrap().expect("CMAF init");
		let fragment = export.next().await.unwrap().expect("CMAF fragment");

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let subscriber = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve());
		import.decode(&init).unwrap();
		import.decode(&fragment).unwrap();

		let snapshot = catalog.snapshot();
		let (name, config) = snapshot.video.renditions.iter().next().expect("video rendition");
		assert!(matches!(config.container, hang::catalog::Container::Cmaf { .. }));
		let mut consumer = Consumer::new(
			&subscriber,
			config,
			name,
			Config {
				kind: Kind::Software,
				..Config::new()
			},
		)
		.await
		.unwrap();

		let frame = consumer.read().await.unwrap().expect("decoded frame");
		assert_eq!(frame.size(), crate::Size::new(320, 240));
	}

	/// A track ends before a decoder that reorders pictures does. The consumer
	/// drains the backend once and returns its tail before reporting the end.
	#[tokio::test]
	async fn track_end_drains_buffered_decoder() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video", hang::container::track_info()).unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		for index in 0..2u64 {
			producer
				.write(moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(index * 33_333).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: index == 0,
				})
				.unwrap();
		}
		producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Config {
				kind: Kind::Named(probe::BUFFERED_NAME.into()),
				..Config::new()
			},
		)
		.await
		.unwrap();

		let mut timestamps = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros());
		}
		assert_eq!(timestamps, vec![0, 33_333]);
		assert!(
			consumer.read().await.unwrap().is_none(),
			"the decoder was drained twice"
		);
	}

	/// A declared discontinuity abandons the previous codec epoch. A delayed
	/// picture from before the seam is drained and discarded before the first new
	/// keyframe is decoded.
	#[tokio::test]
	async fn discontinuity_discards_buffered_tail() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video", hang::container::track_info()).unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		producer
			.write(moq_mux::container::Frame {
				timestamp: Timestamp::from_micros(100_000).unwrap(),
				duration: None,
				payload: Bytes::from_static(b"old access unit"),
				keyframe: true,
			})
			.unwrap();
		producer.discontinuity().unwrap();
		producer
			.write(moq_mux::container::Frame {
				timestamp: Timestamp::ZERO,
				duration: None,
				payload: Bytes::from_static(b"new access unit"),
				keyframe: true,
			})
			.unwrap();
		producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Config {
				kind: Kind::Named(probe::BUFFERED_NAME.into()),
				..Config::new()
			},
		)
		.await
		.unwrap();

		let mut timestamps = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros());
		}
		assert_eq!(timestamps, vec![0]);
	}

	/// Cancellation while a threaded flush is in flight leaves the sink poisoned.
	/// The next read surfaces that error rather than reporting a clean end and
	/// silently discarding the tail.
	#[cfg(not(target_os = "macos"))]
	#[tokio::test]
	async fn cancelled_track_end_flush_is_not_reported_as_drained() {
		probe::prepare_blocking_flush();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video", hang::container::track_info()).unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);
		producer.finish().unwrap();

		let catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"video",
			Config {
				kind: Kind::Named(probe::BLOCKING_FLUSH_NAME.into()),
				..Config::new()
			},
		)
		.await
		.unwrap();

		let mut read = Box::pin(consumer.read());
		let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
		loop {
			tokio::select! {
				_result = &mut read => panic!("flush returned before cancellation"),
				_ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {
					if probe::flush_entered() {
						break;
					}
					if tokio::time::Instant::now() >= deadline {
						probe::release_flush();
						panic!("flush never reached the codec thread");
					}
				}
			}
		}
		drop(read);
		probe::release_flush();

		let err = match consumer.read().await {
			Err(err) => err,
			Ok(_) => panic!("cancelled flush must poison the sink"),
		};
		assert!(err.to_string().contains("cancelled call"), "unexpected error: {err}");
	}

	/// VAAPI returns its buffered tail before the consumer reports track end.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	#[tokio::test]
	async fn the_track_ending_drains_the_decoder() {
		const FRAMES: u64 = 5;
		let config = EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(320, 240, 30)
		};
		let catalog = config.probe().await.expect("probe the software encoder");

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video", hang::container::track_info()).unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);

		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];
		for index in 0..FRAMES {
			if index == 0 {
				encoder.keyframe();
			}
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = crate::Frame::new(surface, moq_net::Timestamp::from_micros(index * 33_333).unwrap());
			for encoded in encoder.encode(&frame).unwrap() {
				producer
					.write(moq_mux::container::Frame {
						timestamp: encoded.timestamp,
						duration: None,
						payload: encoded.payload,
						keyframe: index == 0,
					})
					.unwrap();
			}
		}
		producer.finish().unwrap();

		let decode = Config {
			kind: Kind::Named("vaapi".into()),
			..Config::new()
		};
		// The hardware gate: no libva, no render node, or no H.264 decode
		// entrypoint and the named backend refuses to open.
		let Ok(mut consumer) = Consumer::new(&subscriber, &catalog, "video", decode).await else {
			return;
		};

		let mut timestamps = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
			timestamps.push(frame.timestamp.as_micros());
		}
		let expected: Vec<u128> = (0..FRAMES as u128).map(|index| index * 33_333).collect();
		assert_eq!(timestamps, expected, "the track ended before the stream did");

		// The end stays the end: the drain runs once, so a caller that keeps
		// reading past it does not get the tail a second time.
		assert!(consumer.read().await.unwrap().is_none());
	}
}

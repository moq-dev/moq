//! Subscribe to an encoded H.264, H.265, or AV1 track and emit raw I420 frames.

use std::collections::VecDeque;

use hang::catalog::VideoConfig;

use super::decoder::{Config, Start};
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
		let track = broadcast.track(&name)?;
		let mut subscriber = track
			.subscribe(
				moq_net::track::Subscription::default()
					.with_priority(hang::catalog::PRIORITY.video)
					.with_max_age(config.max_age),
			)
			.await?;
		// A decoder often opens on a track that is already cached: a replacement
		// decoder subscribes while its predecessor still holds groups, and a
		// rendition switched away from and back to stays warm for
		// `TRACK_IDLE_LINGER`. A caller that asked for `Start::Latest` wants
		// none of that backlog, because a cursor starting at sequence zero
		// replays every cached group at decode speed before reaching live
		// media, which on a thirty-second retention is half a minute of pictures raced
		// through.
		//
		// This moves the local read cursor and deliberately not
		// `Subscription::group_start`. That field is a request to the publisher,
		// aggregated across every live subscriber, so naming a stale cached
		// sequence there asks the publisher to rewind the track for everyone
		// reading it. What a player wants is to skip what it already has.
		if config.start == Start::Latest
			&& let Some(live_edge) = track.latest()
		{
			subscriber.start_at(live_edge);
		}
		let track = subscriber;
		// The catalog says how the track is framed, and it is not always the legacy
		// wire: `moq import fmp4` publishes CMAF. Reading a moof+mdat fragment as a
		// varint timestamp plus a payload decodes to garbage rather than failing.
		let container = moq_mux::catalog::hang::Container::try_from(catalog)?;
		let track = moq_mux::container::Consumer::new(track, container);

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
				// The flag goes up only once the tail is in hand, so a read
				// dropped before the drain ran retries it rather than reporting
				// an end the stream has not reached. Flushing twice is safe: the
				// second hands back nothing.
				let tail = self.decoder.flush().await;
				// Set before the error is returned, not after. A flush that
				// failed once fails the same way every time, and the track has
				// ended either way, so leaving the flag down turns one bad
				// drain into a caller that reads, fails, and reads again with
				// nothing in between to wait on. A caller that treats a codec
				// error as one lost picture and carries on then spins.
				self.drained = true;
				self.pending.extend(tail?);
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

	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::Hop::random().into());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(driver.run(moq_tokio::runtime::Runtime::<()>::new()));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}
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

		let origin = produce_origin();
		let requests = origin.dynamic(moq_net::Pattern::all(), Default::default()).unwrap();
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
		// Both frames are encoded before the export runs, so the exporter needs a budget
		// wide enough to read them: its REAL_TIME default keeps only the live edge, and
		// the second `next()` would then block forever waiting for a group that was
		// skipped.
		let mut export =
			moq_mux::container::fmp4::Export::new(source, catalog).with_max_age(std::time::Duration::from_secs(30));
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

	/// A decoder opened on a track that already holds groups starts at the
	/// newest one, not at the oldest still cached.
	///
	/// A player rebuilding its decoder (a backend change, a rendition pin) opens
	/// a second consumer while the first still holds the groups it has not
	/// released. Starting those at sequence zero replays the whole retention at
	/// decode speed before the picture reaches live media.
	#[tokio::test]
	async fn a_second_consumer_starts_at_the_live_edge() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		// Kept so the aggregated subscription can be read back below.
		let published = track.clone();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		// A keyframe opens a group, so this is three groups a second apart.
		for index in 0..3u64 {
			producer
				.write(moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(index * 1_000_000).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: true,
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
				start: Start::Latest,
				// Wide enough to keep every group fresh: the age budget on its own
				// delivers only the live edge, which would pass this test without
				// `Start::Latest` doing anything.
				max_age: std::time::Duration::from_secs(10),
				..Config::new()
			},
		)
		.await
		.unwrap();

		// The buffered probe stamps each picture with the access unit's own
		// timestamp, so this says which group the read started from. It is the
		// backend to use here rather than the plain probe, whose event log is
		// process-wide and belongs to the thread-affinity test.
		let frame = consumer.read().await.unwrap().expect("a decoded frame");
		assert_eq!(
			frame.timestamp,
			Timestamp::from_micros(2_000_000).unwrap(),
			"a fresh consumer replayed the groups an earlier reader still holds",
		);

		// The skip is the local read cursor and nothing else. Asking for it
		// through `Subscription::start` would look equivalent and is not: the
		// floor is aggregated across every live subscriber and tells the
		// publisher what to send, so naming a cached sequence there rewinds the
		// track for everyone reading it. A rendition switched away from and back
		// to is the case that bites, because its cached sequence is stale by
		// then and the publisher resends the broadcast from it.
		assert_eq!(
			published.subscription().and_then(|sub| sub.start),
			None,
			"the publisher was asked to rewind the track",
		);
	}

	/// The default reads everything the track holds.
	///
	/// `Start::Latest` is a player's policy and not the API's: a recorder, an
	/// export, or a test decoding a track that was written before it subscribed
	/// wants every group, and dropping media nobody asked to drop is the worse
	/// of the two mistakes. This is the half that a live-edge default breaks,
	/// so it is pinned beside the other one.
	#[tokio::test]
	async fn the_default_reads_every_cached_group() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		for index in 0..3u64 {
			producer
				.write(moq_mux::container::Frame {
					timestamp: Timestamp::from_micros(index * 1_000_000).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"access unit"),
					keyframe: true,
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
				// The buffered probe rather than the plain one: the plain probe's
				// event log is process-wide and belongs to the thread-affinity test.
				kind: Kind::Named(probe::BUFFERED_NAME.into()),
				// A budget that keeps every group fresh, so the start policy is the
				// only thing deciding what is read.
				max_age: std::time::Duration::from_secs(10),
				..Config::new()
			},
		)
		.await
		.unwrap();

		let mut seen = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
			seen.push(frame.timestamp);
		}
		assert_eq!(
			seen,
			vec![
				Timestamp::from_micros(0).unwrap(),
				Timestamp::from_micros(1_000_000).unwrap(),
				Timestamp::from_micros(2_000_000).unwrap(),
			],
			"the default dropped groups the caller never asked to drop",
		);
	}

	/// A track ends before a decoder that reorders pictures does. The consumer
	/// drains the backend once and returns its tail before reporting the end.
	#[tokio::test]
	async fn track_end_drains_buffered_decoder() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
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
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
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
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
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
		assert!(matches!(err, crate::Error::CodecGone(_)));
		assert!(consumer.read().await.unwrap().is_none());
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
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = broadcast.consume();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);

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

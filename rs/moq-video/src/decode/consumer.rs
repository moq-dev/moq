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
	/// Set once the track has ended and the decoder has been flushed, so the
	/// drain runs exactly once and later reads report the end instead of asking
	/// a track that is already done.
	drained: bool,
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
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"` or `"openh264"`.
	pub fn name(&self) -> &str {
		self.decoder.name()
	}

	/// Read the next decoded I420 frame, or `None` when the track ends.
	///
	/// The end of the track is not the end of the frames: a backend that buffers
	/// is still holding the tail of the stream when the last access unit arrives,
	/// so the track ending flushes the decoder and returns what comes out before
	/// reporting the end.
	///
	/// Not cancel safe. The codec runs on its own thread, so a read dropped while
	/// the decode of an access unit is in flight loses the pictures that decode
	/// produced. Drive it to completion rather than racing it in a `select!`.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		loop {
			if let Some(frame) = self.pending.pop_front() {
				return Ok(Some(frame));
			}
			if self.drained {
				return Ok(None);
			}

			let Some(mux_frame) = self.track.read().await? else {
				// The flag goes up only once the tail is in hand, so a read
				// dropped before the drain ran retries it rather than reporting
				// an end the stream has not reached. Flushing twice is safe: the
				// second hands back nothing.
				let tail = self.decoder.flush().await?;
				self.drained = true;
				self.pending.extend(tail);
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
	use super::*;
	use crate::decode::Kind;
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

	/// Regression: the track ending is not the stream ending. A VAAPI decoder is
	/// still holding the tail of the stream in its DPB when the last access unit
	/// arrives, so `read` has to drain it before reporting the end or every track
	/// stops short of the pictures it published.
	///
	/// Real hardware only, because it is the only backend that holds anything
	/// back: the rest decode one-in one-out and would pass this without a drain
	/// ever running.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	#[tokio::test]
	async fn the_track_ending_drains_the_decoder() {
		use crate::decode::backend::vaapi;

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
			kind: Kind::Named(vaapi::NAME.into()),
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

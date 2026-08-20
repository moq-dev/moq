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
			.subscribe(
				moq_net::track::Subscription::default()
					.with_priority(hang::catalog::PRIORITY.video)
					.with_max_age(config.max_age),
			)
			.await?;
		// The catalog says how the track is framed, and it is not always the legacy
		// wire: `moq import fmp4` publishes CMAF. Reading a moof+mdat fragment as a
		// varint timestamp plus a payload decodes to garbage rather than failing.
		let container = moq_mux::catalog::hang::Container::try_from(&catalog.container)?;
		let track = moq_mux::container::Consumer::new(track, container);

		Ok(Self {
			decoder,
			track,
			pending: VecDeque::new(),
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"` or `"openh264"`.
	pub fn name(&self) -> &str {
		self.decoder.name()
	}

	/// Read the next decoded I420 frame, or `None` when the track ends.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		loop {
			if let Some(frame) = self.pending.pop_front() {
				return Ok(Some(frame));
			}

			let Some(mux_frame) = self.track.read().await? else {
				return Ok(None);
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
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::Origin::random().into());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(driver);
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

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

		let origin = produce_origin();
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
}

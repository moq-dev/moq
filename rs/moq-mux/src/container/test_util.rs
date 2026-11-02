//! Shared scaffolding for exporter tests.

use bytes::{Bytes, BytesMut};
use hang::catalog::{AudioConfig, Container, H264, VideoConfig};
use moq_net::Timestamp;

/// A live single-track Legacy broadcast. All producers stay open, so an
/// exporter sees a stream that has not ended.
pub(crate) struct Live {
	pub(crate) track: crate::container::Producer<crate::catalog::hang::Container>,
	pub(crate) catalog: crate::catalog::Producer,
	consumer: moq_net::broadcast::Consumer,
	broadcast: moq_net::broadcast::Producer,
}

impl Live {
	/// One track named `name`, with `insert` registering its catalog rendition.
	pub(crate) fn new(name: &str, insert: impl FnOnce(&mut crate::catalog::Producer, String)) -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track(
				broadcast.unique_name(name),
				hang::container::track_info(hang::catalog::PRIORITY.video),
			)
			.unwrap();
		insert(&mut catalog, track.name().to_string());
		let kind = if catalog.modify().unwrap().video.renditions.contains_key(track.name()) {
			crate::container::Kind::Video
		} else {
			crate::container::Kind::Audio
		};
		let format = crate::catalog::hang::Container::Legacy(kind);
		Self {
			track: crate::container::Producer::new(track, format),
			catalog,
			consumer,
			broadcast,
		}
	}

	/// Add another track named `name` to the same broadcast, with `insert`
	/// registering its catalog rendition. Used to build an A/V broadcast.
	pub(crate) fn add_track(
		&mut self,
		name: &str,
		insert: impl FnOnce(&mut crate::catalog::Producer, String),
	) -> crate::container::Producer<crate::catalog::hang::Container> {
		let name = self.broadcast.unique_name(name);
		let track = self
			.broadcast
			.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		insert(&mut self.catalog, track.name().to_string());
		crate::container::Producer::new(
			track,
			crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
		)
	}

	/// One Avc3-shape H.264 rendition (320x240 at 30 fps).
	pub(crate) fn avc3() -> Self {
		Self::new(".avc3", |catalog, name| {
			let mut config = VideoConfig::new(H264 {
				profile: 0x42,
				constraints: 0xc0,
				level: 0x1f,
				inline: true,
			});
			config.coded_width = Some(320);
			config.coded_height = Some(240);
			config.framerate = Some(30.0);
			config.container = Container::Legacy;
			catalog.modify().unwrap().video.renditions.insert(name, config);
		})
	}

	/// One Legacy audio rendition.
	pub(crate) fn audio(mut config: AudioConfig) -> Self {
		config.container = Container::Legacy;
		Self::new(".audio", |catalog, name| {
			catalog.modify().unwrap().audio.renditions.insert(name, config);
		})
	}

	pub(crate) async fn source(&self) -> crate::Source {
		crate::source::announced(&self.consumer).await
	}

	pub(crate) async fn catalog_stream(&self) -> crate::catalog::Consumer {
		self.source()
			.await
			.catalog::<()>(crate::catalog::CatalogFormat::Hang)
			.await
			.expect("catalog consumer")
	}
}

/// A catalog broadcast `live` whose H.264 rendition [`Moving::RENDITION`] the catalog can
/// move between tracks: `a` and `b` in `live`, and `a` in the sibling broadcast `other`.
/// Every frame carries a mark byte, so an export shows which track it read ([`marks`]).
pub(crate) struct Moving {
	pub(crate) catalog: crate::catalog::Producer,
	/// `live`'s track `a`, the one the rendition starts on.
	pub(crate) a: crate::container::Producer<crate::catalog::hang::Container>,
	/// `live`'s track `b`.
	pub(crate) b: crate::container::Producer<crate::catalog::hang::Container>,
	/// `other`'s track `a`.
	pub(crate) other: crate::container::Producer<crate::catalog::hang::Container>,
	origin: moq_net::origin::Producer,
	_broadcasts: [moq_net::origin::Publication; 2],
}

impl Moving {
	pub(crate) const RENDITION: &str = "main";
	/// A second SPS, a Main profile one, whose avcC differs from [`SPS`]'s.
	pub(crate) const MAIN_SPS: &[u8] = &[0x67, 0x4d, 0x40, 0x1f, 0xde, 0xad, 0xbe, 0xef];

	pub(crate) fn new() -> Self {
		let origin = crate::source::produce_origin();
		let live = origin.publish("live", Default::default()).unwrap();
		let other = origin.publish("other", Default::default()).unwrap();
		let catalog =
			crate::catalog::Producer::new(&mut live.broadcast().clone(), crate::catalog::Config::default()).unwrap();
		let track = |broadcast: &mut moq_net::broadcast::Producer, name: &str| {
			let info = hang::container::track_info(hang::catalog::PRIORITY.video);
			let track = broadcast.create_track(name, info).unwrap();
			let wire = crate::catalog::hang::Container::Legacy(crate::container::Kind::Video);
			crate::container::Producer::new(track, wire)
		};
		let mut moving = Self {
			a: track(&mut live.broadcast().clone(), "a"),
			b: track(&mut live.broadcast().clone(), "b"),
			other: track(&mut other.broadcast().clone(), "a"),
			catalog,
			origin,
			_broadcasts: [live, other],
		};
		moving.point("a", None, SPS);
		moving
	}

	pub(crate) async fn source(&self) -> crate::Source {
		crate::source::resolved(&self.origin, "live").await
	}

	pub(crate) async fn catalog_stream(&self) -> crate::catalog::Consumer {
		let source = self.source().await;
		source.catalog::<()>(crate::catalog::CatalogFormat::Hang).await.unwrap()
	}

	/// Point the rendition at `track` in `broadcast` (`live` when `None`), described by `sps`.
	pub(crate) fn point(&mut self, track: &str, broadcast: Option<&str>, sps: &'static [u8]) {
		let config = Self::config(track, broadcast, sps);
		let mut catalog = self.catalog.modify().unwrap();
		catalog.video.renditions.insert(Self::RENDITION.to_string(), config);
	}

	/// The rendition's catalog entry reading `track` in `broadcast`, described by `sps`.
	pub(crate) fn config(track: &str, broadcast: Option<&str>, sps: &'static [u8]) -> VideoConfig {
		let mut config = VideoConfig::new(H264 {
			profile: sps[1],
			constraints: sps[2],
			level: sps[3],
			inline: false,
		});
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.framerate = Some(30.0);
		config.container = Container::Legacy;
		config.description =
			Some(crate::codec::h264::build_avcc(&[Bytes::from_static(sps)], &[Bytes::from_static(PPS)]).unwrap());
		config.track = Some(track.to_string());
		config.broadcast = broadcast.map(|rel| moq_net::path::Relative::new(rel).unwrap());
		config
	}
}

/// One 30 fps length-prefixed H.264 frame `index` frames in, a keyframe opening each second,
/// whose slice carries `mark`.
pub(crate) fn marked_frame(index: u64, mark: u8) -> crate::container::Frame {
	let keyframe = index.is_multiple_of(30);
	let nal = [if keyframe { 0x65 } else { 0x41 }, 0x88, mark, mark, mark, mark, 0x80];
	let mut payload = BytesMut::new();
	payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
	payload.extend_from_slice(&nal);
	crate::container::Frame {
		timestamp: Timestamp::from_micros(index * 1_000_000 / 30).unwrap(),
		payload: payload.freeze(),
		keyframe,
		duration: None,
	}
}

/// The mark of each [`marked_frame`] slice in `bytes`, in order.
pub(crate) fn marks(bytes: &[u8]) -> Vec<u8> {
	bytes
		.windows(6)
		.filter(|w| matches!(w[0], 0x65 | 0x41) && w[1] == 0x88 && w[2] != 0 && w[2..].iter().all(|b| *b == w[2]))
		.map(|w| w[2])
		.collect()
}

/// H.264 NALs used by [`video_frame`], exposed for tests that assert on them.
pub(crate) const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0xde, 0xad, 0xbe, 0xef];
pub(crate) const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
pub(crate) const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x21, 0x00, 0x11, 0x22, 0x33];
const DELTA: &[u8] = &[0x41, 0x9a, 0x00, 0x01];

/// One Annex-B H.264 frame with no duration: SPS + PPS + IDR for a keyframe,
/// otherwise a single delta slice.
pub(crate) fn video_frame(timestamp_us: u64, keyframe: bool) -> crate::container::Frame {
	let nals: &[&[u8]] = if keyframe { &[SPS, PPS, IDR] } else { &[DELTA] };
	let mut payload = BytesMut::new();
	for nal in nals {
		payload.extend_from_slice(&[0, 0, 0, 1]);
		payload.extend_from_slice(nal);
	}
	crate::container::Frame {
		timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
		payload: payload.freeze(),
		keyframe,
		duration: None,
	}
}

/// One frame with a fixed payload and no duration.
pub(crate) fn raw_frame(timestamp_us: u64, payload: &'static [u8], keyframe: bool) -> crate::container::Frame {
	crate::container::Frame {
		timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
		payload: Bytes::from_static(payload),
		keyframe,
		duration: None,
	}
}

/// Every frame timestamp, in micros, that each media rendition in `catalog` published, by track.
///
/// Reads through each rendition's own container, so an fMP4 timestamp comes from the fragment's
/// `tfdt` rather than the wire header. The importer must have finished its tracks.
pub(crate) async fn published<E>(
	consumer: &moq_net::broadcast::Consumer,
	catalog: &hang::catalog::Catalog<E>,
) -> std::collections::BTreeMap<String, Vec<u128>> {
	let mut containers = Vec::new();
	for (name, config) in &catalog.video.renditions {
		containers.push((name.clone(), crate::catalog::hang::Container::try_from(config).unwrap()));
	}
	for (name, config) in &catalog.audio.renditions {
		containers.push((name.clone(), crate::catalog::hang::Container::try_from(config).unwrap()));
	}

	let mut out = std::collections::BTreeMap::new();
	for (name, container) in containers {
		let replay = moq_net::track::Subscription::default().with_max_delay(std::time::Duration::from_secs(3600));
		let track = consumer.track(&name).unwrap().subscribe(replay).await.unwrap();
		let mut reader = crate::container::Consumer::new(track, container);
		let mut timestamps = Vec::new();
		while let Some(frame) = kio::clock::timeout(std::time::Duration::from_secs(5), reader.read_frame())
			.await
			.expect("the importer finished its tracks")
			.unwrap()
		{
			timestamps.push(frame.timestamp.as_micros());
		}
		out.insert(name, timestamps);
	}
	out
}

/// The root clocks a broadcast's catalog snapshots carry, read off its `catalog.json` track.
pub(crate) struct Clocks(crate::catalog::hang::Consumer);

impl Clocks {
	/// Subscribe before importing, so the first snapshot is not missed.
	pub(crate) async fn subscribe(consumer: &moq_net::broadcast::Consumer) -> Self {
		let track = consumer
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		Self(crate::catalog::hang::Consumer::new(track))
	}

	/// The clock of every snapshot published since the last call, without waiting.
	pub(crate) fn drain(&mut self) -> Vec<Option<hang::catalog::Clock>> {
		let waiter = kio::Waiter::noop();
		let mut clocks = Vec::new();
		while let std::task::Poll::Ready(catalog) = self.0.poll_next(&waiter) {
			let Some(catalog) = catalog.unwrap() else { break };
			clocks.push(catalog.clock);
		}
		clocks
	}
}

/// The one offset every published timestamp moved by between two imports of the same media,
/// within a tick of rounding: one mapping for every track, so A/V sync and B-frame order survive
/// exactly.
pub(crate) fn common_offset(
	verbatim: &std::collections::BTreeMap<String, Vec<u128>>,
	live: &std::collections::BTreeMap<String, Vec<u128>>,
) -> i128 {
	assert_eq!(verbatim.keys().count(), live.keys().count(), "the same tracks publish");
	let mut offset = None;
	for (v, l) in verbatim.values().zip(live.values()) {
		assert_eq!(v.len(), l.len(), "the same frames publish");
		for (v, l) in v.iter().zip(l) {
			let delta = *l as i128 - *v as i128;
			let first = *offset.get_or_insert(delta);
			assert!(
				(delta - first).abs() <= 1_000,
				"every frame moves by one offset: {delta} vs {first}"
			);
		}
	}
	offset.expect("frames were published")
}

/// An origin at `hop`, its driver spawned.
pub(crate) fn origin(hop: u64) -> moq_net::origin::Producer {
	let config = moq_net::origin::Config::new(moq_net::Hop::new(hop).unwrap());
	let (producer, driver) = moq_net::origin::Producer::new(config);
	tokio::spawn(moq_net::time::run(driver));
	producer
}

/// A lite-07 session over memory carrying `from`'s broadcasts into `to`, open while held.
pub(crate) struct Link {
	_sessions: (moq_net::Session, moq_net::Session),
	gate: Gate,
}

impl Link {
	/// Link `from` into `to`, which accepts every stream.
	pub(crate) async fn new(from: &moq_net::origin::Producer, to: &moq_net::origin::Producer) -> Self {
		Self::held(from, to, usize::MAX).await
	}

	/// Link `from` into `to`, which accepts only its first `uni` uni streams until
	/// [`open`](Self::open), as a network can deliver one group stream well ahead of the
	/// rest. SETUP takes the first.
	pub(crate) async fn held(from: &moq_net::origin::Producer, to: &moq_net::origin::Producer, uni: usize) -> Self {
		use moq_net::transport::memory;

		let version: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
		let mut config = memory::Config::default();
		config.protocol = Some(version.alpn().to_string());
		let (client_transport, server_transport) = memory::pair(config);
		let gate = Gate(std::sync::Arc::new(std::sync::Mutex::new((
			uni,
			kio::WaiterList::new(),
		))));
		let server_transport = Gated {
			inner: server_transport,
			gate: gate.clone(),
			answers: true,
		};
		let client = moq_net::Client::new()
			.with_versions(version.into())
			.with_publisher(from.consume());
		let server = moq_net::Server::new()
			.with_versions(version.into())
			.with_subscriber(to.clone());
		// Each driver runs as soon as its handshake resolves: the accept waits on the client's SETUP.
		let sessions = tokio::join!(
			async {
				let (session, driver) = client
					.connect(moq_net::time::Instant::now(), client_transport)
					.await
					.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			},
			async {
				let (session, driver) = server
					.accept(moq_net::time::Instant::now(), server_transport)
					.await
					.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			}
		);
		Self {
			_sessions: sessions,
			gate,
		}
	}

	/// A lazily discovering client session subscribing into `to` from a server publishing
	/// `from`. A server that never `answers` accepts no request stream, as a stalled peer.
	pub(crate) async fn lazy(from: &moq_net::origin::Producer, to: &moq_net::origin::Producer, answers: bool) -> Self {
		use moq_net::transport::memory;

		let version: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
		let mut config = memory::Config::default();
		config.protocol = Some(version.alpn().to_string());
		let (client_transport, server_transport) = memory::pair(config);
		let gate = Gate(std::sync::Arc::new(std::sync::Mutex::new((
			usize::MAX,
			kio::WaiterList::new(),
		))));
		let server_transport = Gated {
			inner: server_transport,
			gate: gate.clone(),
			answers,
		};
		let client = moq_net::Client::new()
			.with_versions(version.into())
			.with_subscriber(to.clone());
		let server = moq_net::Server::new()
			.with_versions(version.into())
			.with_publisher(from.consume());
		let sessions = tokio::join!(
			async {
				let (session, driver) = client
					.connect(moq_net::time::Instant::now(), client_transport)
					.await
					.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			},
			async {
				let (session, driver) = server
					.accept(moq_net::time::Instant::now(), server_transport)
					.await
					.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			}
		);
		Self {
			_sessions: sessions,
			gate,
		}
	}

	/// Accept every stream from now on, the held ones first.
	pub(crate) fn open(&self) {
		let mut gate = self.gate.0.lock().unwrap();
		gate.0 = usize::MAX;
		gate.1.wake();
	}
}

/// The uni streams a [`Gated`] session may still accept, and the accepts parked on it.
#[derive(Clone)]
struct Gate(std::sync::Arc<std::sync::Mutex<(usize, kio::WaiterList)>>);

/// A memory session that accepts uni streams only while its [`Gate`] allows.
#[derive(Clone)]
struct Gated {
	inner: moq_net::transport::memory::Session,
	gate: Gate,
	/// Whether it accepts the peer's request (bidi) streams at all.
	answers: bool,
}

impl moq_net::transport::poll::Session for Gated {
	type SendStream = <moq_net::transport::memory::Session as moq_net::transport::poll::Session>::SendStream;
	type RecvStream = <moq_net::transport::memory::Session as moq_net::transport::poll::Session>::RecvStream;
	type Error = <moq_net::transport::memory::Session as moq_net::transport::poll::Session>::Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> std::task::Poll<Result<Self::RecvStream, Self::Error>> {
		{
			let mut gate = self.gate.0.lock().unwrap();
			if gate.0 == 0 {
				waiter.register(&mut gate.1);
				return std::task::Poll::Pending;
			}
		}
		let accepted = std::task::ready!(self.inner.poll_accept_uni(waiter));
		let mut gate = self.gate.0.lock().unwrap();
		gate.0 = gate.0.saturating_sub(1);
		std::task::Poll::Ready(accepted)
	}

	fn poll_accept_bi(
		&self,
		waiter: &kio::Waiter,
	) -> std::task::Poll<Result<moq_net::transport::poll::BiStreams<Self>, Self::Error>> {
		if !self.answers {
			return std::task::Poll::Pending;
		}
		self.inner.poll_accept_bi(waiter)
	}

	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: moq_net::transport::poll::SendOptions<'_, moq_net::transport::poll::Ungrouped>,
	) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
		self.inner.poll_open_uni(waiter, options)
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: moq_net::transport::poll::SendOptions<'_, moq_net::transport::poll::Ungrouped>,
	) -> std::task::Poll<Result<moq_net::transport::poll::BiStreams<Self>, Self::Error>> {
		self.inner.poll_open_bi(waiter, options)
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: moq_net::transport::poll::SendOptions<
			'_,
			<Self::SendStream as moq_net::transport::poll::SendStream>::Group,
		>,
	) -> std::task::Poll<Result<Option<moq_net::transport::poll::BiStreams<Self>>, Self::Error>> {
		self.inner.poll_open_request(waiter, options)
	}

	fn send_group(&self, priority: i64) -> Option<moq_net::transport::poll::Ungrouped> {
		self.inner.send_group(priority)
	}

	fn send_datagram(
		&self,
		payload: Bytes,
		options: moq_net::transport::poll::SendOptions<'_, moq_net::transport::poll::Ungrouped>,
	) -> Result<(), Self::Error> {
		self.inner.send_datagram(payload, options)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> std::task::Poll<Result<Bytes, Self::Error>> {
		self.inner.poll_recv_datagram(waiter)
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.protocol()
	}

	fn address(&self) -> moq_net::transport::Address {
		self.inner.address()
	}

	fn close(&self, code: u32, reason: &str) {
		self.inner.close(code, reason)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> std::task::Poll<Self::Error> {
		self.inner.poll_closed(waiter)
	}

	fn metrics(&self) -> impl moq_net::transport::Metrics {
		self.inner.metrics()
	}
}

//! `moqsrc` following a broadcast through a relay, end to end: a pipeline playing a path over a
//! real QUIC session while its publishers come, go, and replace each other.
//!
//! The relay is a server sharing one origin between every session, on its own runtime. Publishers
//! publish into that origin directly, or over a session of their own where the test is about the
//! session. The pipeline renders through a synced sink with a video sink's lateness budget, so a
//! stream that starts behind the clock drops instead of rendering.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, mpsc};
use std::time::{Duration, Instant};

use gst::prelude::*;
use hang::catalog::{Container, H264, VideoConfig};
use moq_net::Epoch;
use moq_net::origin::Route;

const TIMEOUT: Duration = Duration::from_secs(10);
const FRAME: Duration = Duration::from_millis(33);
/// Enough rendered buffers to call a stream flowing.
const FLOWING: usize = 10;
/// Room to connect or switch before a stream renders at its frame rate. A sink dropping late
/// buffers still renders one a second, so a stream behind the clock can't keep up.
const FLOW_TIMEOUT: Duration = Duration::from_secs(5);

fn init() {
	static INIT: Once = Once::new();
	INIT.call_once(|| {
		gst::init().unwrap();
		gstmoq::plugin_register_static().expect("register moq plugin");
	});
}

/// A new publisher instance.
fn route() -> Route {
	Route::default().with_epoch(Epoch::mint())
}

fn h264() -> VideoConfig {
	let mut config = VideoConfig::new(H264 {
		profile: 0x42,
		constraints: 0x00,
		level: 0x1f,
		inline: false,
	});
	config.container = Container::Legacy;
	config
}

/// [`h264`] with its parameter sets out of band, which changes its caps.
fn avc() -> VideoConfig {
	let mut config = h264();
	config.description = Some(bytes::Bytes::from_static(&[1, 0x42, 0x00, 0x1f, 0xff, 0xe0, 0x00]));
	config
}

/// A relay: one origin shared by every session it accepts.
struct Relay {
	runtime: Option<tokio::runtime::Runtime>,
	origin: moq_net::origin::Producer,
	port: u16,
}

impl Relay {
	fn start() -> Self {
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(2)
			.enable_all()
			.build()
			.expect("build relay runtime");
		let (origin, port) = runtime.block_on(async {
			let origin = moq_tokio::origin::spawn();
			let mut config = moq_tokio::listen::Config::default();
			config.bind = Some("127.0.0.1:0".parse().unwrap());
			config.tls.generate = vec!["localhost".into()];
			let mut server = config
				.init(Default::default())
				.expect("server init")
				.listen()
				.await
				.expect("listen");
			let port = server.local_addr().expect("local addr").port();

			let shared = origin.clone();
			tokio::spawn(async move {
				while let Some(request) = server.accept().await {
					let origin = shared.clone();
					tokio::spawn(async move {
						if let Ok(session) = request
							.with_publisher(&origin)
							.with_subscriber(origin.clone())
							.ok()
							.await
						{
							session.closed().await;
						}
					});
				}
			});
			(origin, port)
		});

		Self {
			runtime: Some(runtime),
			origin,
			port,
		}
	}

	fn url(&self) -> String {
		format!("https://127.0.0.1:{}/", self.port)
	}

	fn runtime(&self) -> &tokio::runtime::Runtime {
		self.runtime.as_ref().expect("relay running")
	}

	/// Publish `path` straight into the relay.
	fn publish(&self, path: &str, route: Route, config: VideoConfig, tag: u8) -> Publisher {
		Publisher::announce(&self.origin, path, route, config, tag)
	}

	/// Serve every path under `prefix` with one broadcast, as a dynamic route.
	fn serve(&self, prefix: &str, route: Route, config: VideoConfig, tag: u8) -> Served {
		let publisher = Publisher::new(moq_net::broadcast::Info::new().produce(), config, tag);
		let dynamic = self.origin.dynamic(prefix, route).expect("serve the prefix");
		let broadcast = publisher.broadcast.consume();
		let handler = self.runtime().spawn(async move {
			while let Ok(request) = dynamic.requested_broadcast().await {
				request.accept(&broadcast);
			}
		});
		Served {
			_publisher: publisher,
			handler,
		}
	}

	/// Publish `path` from another origin, over a session of its own.
	fn connect(&self, path: &str, route: Route, config: VideoConfig, tag: u8) -> Remote {
		let _runtime = self.runtime().enter();
		let origin = moq_tokio::origin::spawn();
		let publisher = Publisher::announce(&origin, path, route, config, tag);
		let mut client = moq_tokio::connect::Config::default();
		client.tls.insecure = Some(true);
		let client = client.init(Default::default()).expect("client init");
		let url: url::Url = self.url().parse().unwrap();
		let connection = self.runtime().block_on(async {
			tokio::time::timeout(
				TIMEOUT,
				client
					.with_publisher(&origin)
					.with_reconnect(false)
					.connect(url)
					.established(),
			)
			.await
			.expect("the publisher never connected")
			.expect("publisher connects")
		});
		Remote {
			connection,
			_publisher: publisher,
			_origin: origin,
		}
	}

	/// Run `end`, then wait for nothing to serve `path` at the relay.
	fn offline(&self, path: &str, end: impl FnOnce()) {
		let mut follow = self.origin.consume().follow(path).expect("follow the path");
		let mut next = || {
			self.runtime()
				.block_on(async { tokio::time::timeout(TIMEOUT, follow.next()).await })
				.expect("the path never changed")
				.expect("relay origin closed")
		};
		assert!(
			matches!(next(), moq_net::announce::Event::Start(_)),
			"the path was not served"
		);
		end();
		while !matches!(next(), moq_net::announce::Event::End(_)) {}
	}
}

impl Drop for Relay {
	fn drop(&mut self) {
		if let Some(runtime) = self.runtime.take() {
			runtime.shutdown_background();
		}
	}
}

/// Frames written in real time on their own thread, each payload filled with its publisher's tag.
struct Feed {
	stop: Arc<AtomicBool>,
	thread: Option<std::thread::JoinHandle<moq_mux::container::Producer<moq_mux::catalog::hang::Container>>>,
}

impl Feed {
	fn start(mut producer: moq_mux::container::Producer<moq_mux::catalog::hang::Container>, tag: u8) -> Self {
		let stop = Arc::new(AtomicBool::new(false));
		let stopped = stop.clone();
		let thread = std::thread::spawn(move || {
			let start = Instant::now();
			for i in 0u32.. {
				if stopped.load(Ordering::Relaxed) {
					break;
				}
				let frame = moq_mux::container::Frame {
					timestamp: moq_net::Timestamp::from_micros((FRAME * i).as_micros() as u64).unwrap(),
					payload: bytes::Bytes::from(vec![tag; 64]),
					keyframe: i % 30 == 0,
					duration: None,
				};
				// A track its session dropped refuses the rest, which is the session's to report.
				if producer.write(frame).is_err() {
					break;
				}
				std::thread::sleep((start + FRAME * (i + 1)).saturating_duration_since(Instant::now()));
			}
			producer
		});
		Self {
			stop,
			thread: Some(thread),
		}
	}

	/// Stop writing, returning the producer to end the track with.
	fn stop(mut self) -> moq_mux::container::Producer<moq_mux::catalog::hang::Container> {
		self.stop.store(true, Ordering::Relaxed);
		self.thread.take().unwrap().join().expect("feed panicked")
	}
}

impl Drop for Feed {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::Relaxed);
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

/// A broadcast with one video rendition, fed in real time.
struct Publisher {
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	feed: Option<Feed>,
}

impl Publisher {
	fn new(mut broadcast: moq_net::broadcast::Producer, config: VideoConfig, tag: u8) -> Self {
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).expect("catalog");
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.expect("video track");
		{
			let mut guard = catalog.modify().unwrap();
			guard.video.renditions = BTreeMap::from([("video".to_string(), config)]);
		}
		let producer = moq_mux::container::Producer::new(
			video,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		Self {
			broadcast,
			catalog,
			feed: Some(Feed::start(producer, tag)),
		}
	}

	fn announce(origin: &moq_net::origin::Producer, path: &str, route: Route, config: VideoConfig, tag: u8) -> Self {
		let publisher = Self::new(origin.create_broadcast(path).expect("create broadcast"), config, tag);
		publisher.broadcast.announce(route).expect("announce");
		publisher
	}

	/// End the broadcast the way `moqsink` does: the media, then the catalog, then the
	/// announcement.
	fn end(mut self) {
		let mut media = self.feed.take().unwrap().stop();
		media.finish().expect("finish the media");
		std::thread::sleep(Duration::from_millis(100));
		self.catalog.finish().expect("finish the catalog");
		self.broadcast.unannounce();
	}
}

/// A broadcast served under a prefix until dropped.
struct Served {
	_publisher: Publisher,
	handler: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
	fn drop(&mut self) {
		self.handler.abort();
	}
}

/// A publisher on a session of its own.
struct Remote {
	connection: moq_tokio::Connection,
	_publisher: Publisher,
	_origin: moq_net::origin::Producer,
}

impl Drop for Remote {
	fn drop(&mut self) {
		self.connection.abort(moq_net::Error::Cancel);
	}
}

/// `moqsrc` playing a path into a synced sink.
struct Player {
	pipeline: gst::Pipeline,
	/// How many video pads `moqsrc` added.
	added: Arc<AtomicUsize>,
	/// The stream-start, caps, and EOS events that reached the sink, in order.
	events: Arc<Mutex<Vec<String>>>,
	/// The publisher tag of every buffer the sink rendered.
	rendered: mpsc::Receiver<u8>,
}

impl Player {
	fn start(url: &str, broadcast: &str) -> Self {
		init();
		let pipeline = gst::Pipeline::new();
		let src = gst::ElementFactory::make("moqsrc")
			.property("url", url)
			.property("broadcast", broadcast)
			.property("tls-disable-verify", true)
			.build()
			.expect("create moqsrc");
		let sink = gst::ElementFactory::make("fakesink")
			.property("sync", true)
			.property("max-lateness", gst::ClockTime::from_mseconds(20).nseconds() as i64)
			.property("qos", true)
			.property("signal-handoffs", true)
			.build()
			.expect("create fakesink");
		pipeline.add_many([&src, &sink]).unwrap();

		// Linked the way a pipeline links by name: the first video pad, once.
		let added = Arc::new(AtomicUsize::new(0));
		let sink_pad = sink.static_pad("sink").unwrap();
		let (count, link) = (added.clone(), sink_pad.clone());
		src.connect_pad_added(move |_, pad| {
			if !pad.name().starts_with("video_") {
				return;
			}
			count.fetch_add(1, Ordering::SeqCst);
			if !link.is_linked() {
				pad.link(&link).expect("link the video pad");
			}
		});

		let events = Arc::new(Mutex::new(Vec::new()));
		let seen = events.clone();
		sink_pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
			if let Some(gst::PadProbeData::Event(event)) = &info.data {
				let event = match event.view() {
					gst::EventView::StreamStart(_) => "stream-start".to_string(),
					gst::EventView::Caps(caps) => caps.caps().to_string(),
					gst::EventView::Eos(_) => "eos".to_string(),
					_ => return gst::PadProbeReturn::Ok,
				};
				seen.lock().unwrap().push(event);
			}
			gst::PadProbeReturn::Ok
		});

		let (tx, rendered) = mpsc::channel();
		sink.connect("handoff", false, move |values| {
			let buffer = values[1].get::<gst::Buffer>().expect("handoff buffer");
			let tag = buffer.map_readable().expect("readable buffer")[0];
			let _ = tx.send(tag);
			None
		});

		pipeline.set_state(gst::State::Playing).expect("play");
		Self {
			pipeline,
			added,
			events,
			rendered,
		}
	}

	/// Wait for the sink to render `count` buffers from the publisher tagged `tag`, at the rate a
	/// stream on time renders.
	fn rendered(&self, tag: u8, count: usize) {
		let deadline = Instant::now() + FLOW_TIMEOUT + FRAME * count as u32;
		let mut seen = 0;
		while seen < count {
			match self
				.rendered
				.recv_timeout(deadline.saturating_duration_since(Instant::now()))
			{
				Ok(rendered) if rendered == tag => seen += 1,
				Ok(_) => {}
				Err(_) => panic!(
					"rendered {seen}/{count} buffers from publisher {tag}; errors: {:?}",
					self.errors()
				),
			}
		}
	}

	fn events(&self) -> Vec<String> {
		self.events.lock().unwrap().clone()
	}

	fn stream_starts(&self) -> usize {
		self.events().iter().filter(|event| *event == "stream-start").count()
	}

	fn errors(&self) -> Vec<String> {
		let bus = self.pipeline.bus().unwrap();
		std::iter::from_fn(|| bus.pop_filtered(&[gst::MessageType::Error]))
			.map(|message| match message.view() {
				gst::MessageView::Error(err) => format!("{} ({:?})", err.error(), err.debug()),
				_ => unreachable!(),
			})
			.collect()
	}

	/// Every stream played on the one pad, with no EOS and no error.
	fn assert_one_pad(&self) {
		assert_eq!(self.added.load(Ordering::SeqCst), 1, "moqsrc added another pad");
		let events = self.events();
		assert!(!events.iter().any(|event| event == "eos"), "a pad ended: {events:?}");
		assert_eq!(self.errors(), Vec::<String>::new());
	}
}

impl Drop for Player {
	fn drop(&mut self) {
		let _ = self.pipeline.set_state(gst::State::Null);
	}
}

/// A newer publisher instance takes the path while the old one is still publishing. The pipeline
/// cuts over to it on the pad it linked, starting the new stream at the clock's running time so a
/// synced sink renders it.
#[test]
fn a_newer_instance_takes_over_while_the_old_one_stays_up() {
	let relay = Relay::start();
	let player = Player::start(&relay.url(), "live");

	let _old = relay.publish("live", route(), h264(), 1);
	player.rendered(1, FLOWING);

	let _new = relay.publish("live", route(), h264(), 2);
	player.rendered(2, FLOWING);

	player.assert_one_pad();
	assert_eq!(player.stream_starts(), 2);
}

/// Without epochs, the path changing hands is still another instance.
#[test]
fn an_epochless_republish_switches() {
	let relay = Relay::start();
	let player = Player::start(&relay.url(), "live");

	let old = relay.publish("live", Route::default(), h264(), 1);
	player.rendered(1, FLOWING);

	let _new = relay.publish("live", Route::default(), h264(), 2);
	drop(old);
	player.rendered(2, FLOWING);

	player.assert_one_pad();
	assert_eq!(player.stream_starts(), 2);
}

/// A route over a prefix serves the path as much as one at the path does, so another instance of
/// it is a switch too.
#[test]
fn a_restarted_prefix_switches() {
	let relay = Relay::start();
	let player = Player::start(&relay.url(), "pool/live");

	let _old = relay.serve("pool", route(), h264(), 1);
	player.rendered(1, FLOWING);

	let _new = relay.serve("pool", route(), h264(), 2);
	player.rendered(2, FLOWING);

	player.assert_one_pad();
	assert_eq!(player.stream_starts(), 2);
}

/// Re-pricing the same instance changes nothing about what plays. Updates are delivered in order,
/// so a later instance's switch is counted after any the update could have caused.
#[test]
fn an_update_does_not_switch() {
	let relay = Relay::start();
	let player = Player::start(&relay.url(), "live");

	let old = relay.publish("live", route(), h264(), 1);
	player.rendered(1, FLOWING);

	old.broadcast
		.announce(old.broadcast.route().expect("announced").with_cost(5))
		.expect("re-price");
	// Past the origin's update hold, so the update is not folded into the instance after it.
	player.rendered(1, 3 * FLOWING);

	let _new = relay.publish("live", route(), h264(), 2);
	player.rendered(2, FLOWING);

	player.assert_one_pad();
	assert_eq!(player.stream_starts(), 2, "the update switched: {:?}", player.events());
}

/// The instance that takes over can carry other caps: they go out on the same pad, for
/// downstream to accept or refuse.
#[test]
fn a_switch_with_new_caps_keeps_the_pad() {
	let relay = Relay::start();
	let player = Player::start(&relay.url(), "live");

	let _old = relay.publish("live", route(), h264(), 1);
	player.rendered(1, FLOWING);

	let _new = relay.publish("live", route(), avc(), 2);
	player.rendered(2, FLOWING);

	player.assert_one_pad();
	let caps: Vec<String> = player
		.events()
		.into_iter()
		.filter(|event| event.starts_with("video/"))
		.collect();
	assert_eq!(caps.len(), 2, "{caps:?}");
	assert!(caps[0].contains("annexb"), "{caps:?}");
	assert!(caps[1].contains("avc"), "{caps:?}");
}

/// A broadcast that ends holds its pads without EOS, and the next one to start resumes on them.
#[test]
fn an_end_then_a_start_resumes_on_the_same_pad() {
	let relay = Relay::start();
	let player = Player::start(&relay.url(), "live");

	let old = relay.publish("live", route(), h264(), 1);
	player.rendered(1, FLOWING);

	relay.offline("live", || old.end());
	// Nothing serves the path for a while.
	std::thread::sleep(Duration::from_millis(300));

	let _new = relay.publish("live", route(), h264(), 2);
	player.rendered(2, FLOWING);

	player.assert_one_pad();
	assert_eq!(player.stream_starts(), 2);
}

/// A publisher whose session closes without finishing anything is the source going away, not a
/// broken stream: the pads hold for the next publisher.
#[test]
fn a_closed_publisher_session_resumes_on_the_next_start() {
	let relay = Relay::start();
	let player = Player::start(&relay.url(), "live");

	let old = relay.connect("live", route(), h264(), 1);
	player.rendered(1, FLOWING);

	relay.offline("live", || old.connection.abort(moq_net::Error::Cancel));
	// Nothing serves the path for a while.
	std::thread::sleep(Duration::from_millis(300));

	let _new = relay.connect("live", route(), h264(), 2);
	player.rendered(2, FLOWING);

	player.assert_one_pad();
	assert_eq!(player.stream_starts(), 2);
}

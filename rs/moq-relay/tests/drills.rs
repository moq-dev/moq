//! Transport and lifecycle failure drills against a real relay.
//!
//! Delivery working on a healthy loopback says nothing about cancellation, a
//! relay dying mid-group, or a publisher coming back. Each drill here disrupts a
//! live session over real QUIC and asserts three things:
//!
//! 1. the fault actually activated, recorded as a value the test prints;
//! 2. the failure surfaced as a terminal result, never a clean finish or a hang;
//! 3. the resources it owned were released.
//!
//! `no_publisher_never_delivers` is the negative control: the same harness with
//! nothing publishing must report failure, so a drill that passes because no
//! data ever moved cannot hide here.
//!
//! See `test/drill/README.md` for the recipe and the loom/fuzz cases covering
//! the primitives underneath.
#![cfg(all(feature = "quinn", feature = "websocket"))]

use std::time::Duration;

use moq_native::moq_net::{self, Origin};
use moq_relay::{Config, PublicConfig, PublicDetailed, Relay};

/// Ceiling for anything a drill waits on. Every wait is bounded, so a broken
/// handoff fails as a timeout with a message instead of hanging the suite.
const TIMEOUT: Duration = Duration::from_secs(10);

/// How long a stalled reader sits on its hands, and how far the publisher runs
/// ahead of it. Long enough that the backlog is unambiguous, short enough to
/// stay a unit test.
const STALL: Duration = Duration::from_millis(300);
const STALL_GROUPS: usize = 8;

/// The track every drill publishes.
const TRACK: &str = "drill";

/// A relay on its own runtime, so a drill can kill it the way a crash does.
///
/// Aborting the `run` task is not enough: [`moq_relay::serve`] spawns a task per
/// connection, and those keep serving a relay whose accept loop is gone. Owning
/// the runtime means one shutdown takes the accept loop, every connection task,
/// and the UDP socket at once.
struct RelayHost {
	port: u16,
	/// `None` once killed, which is also what stops [`Drop`] from killing twice.
	runtime: Option<tokio::runtime::Runtime>,
}

impl RelayHost {
	/// Bind a relay and wait for it to be ready to accept.
	async fn start(requested_port: Option<u16>) -> Self {
		// Process-global; every drill in this binary races to be first.
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		let mut config = Config::default();
		config.server.bind = Some(format!("127.0.0.1:{}", requested_port.unwrap_or_default()));
		config.server.tls.generate = vec!["localhost".into()];
		config.auth.public = Some(PublicConfig::Detailed(PublicDetailed {
			subscribe: vec![String::new()],
			publish: vec![String::new()],
			api: None,
		}));

		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(2)
			.enable_all()
			.build()
			.expect("build relay runtime");

		let (ready, bound) = tokio::sync::oneshot::channel();
		runtime.spawn(async move {
			match Relay::load(config).await {
				Ok(relay) => {
					let _ = ready.send(Ok(relay.addr));
					let _ = relay.run().await;
				}
				Err(err) => {
					let _ = ready.send(Err(format!("{err:#}")));
				}
			}
		});

		let addr = tokio::time::timeout(TIMEOUT, bound)
			.await
			.expect("relay startup timed out")
			.expect("relay task vanished during startup")
			.expect("relay failed to load");
		let port = addr.expect("relay did not bind UDP").port();
		if let Some(requested_port) = requested_port {
			assert_eq!(port, requested_port, "relay bound the wrong port");
		}

		Self {
			port,
			runtime: Some(runtime),
		}
	}

	/// Kill the relay: drop the runtime, taking every task and socket with it.
	///
	/// The shutdown blocks, so it runs on a blocking thread rather than in the
	/// test's own runtime. The timeout only bounds how long a stuck worker
	/// delays the test; the tasks are already gone when it expires.
	async fn kill(&mut self) {
		let runtime = self.runtime.take().expect("relay already killed");
		tokio::task::spawn_blocking(move || runtime.shutdown_timeout(Duration::from_secs(1)))
			.await
			.expect("relay shutdown panicked");
	}

	/// The URL clients dial. `https` is WebTransport over QUIC.
	fn url(&self) -> url::Url {
		format!("https://127.0.0.1:{}/drill", self.port)
			.parse()
			.expect("parse relay url")
	}
}

impl Drop for RelayHost {
	fn drop(&mut self) {
		// A drill that ends (or panics) with the relay still up must not leave
		// its threads and socket behind for the next one to trip over.
		if let Some(runtime) = self.runtime.take() {
			runtime.shutdown_background();
		}
	}
}

/// A client that trusts the relay's generated certificate and speaks QUIC only.
///
/// The WebSocket fallback is off on purpose: these drills are about the QUIC
/// path, and a silent fallback would grade a different transport than the one
/// under test.
fn client_config(url: &url::Url) -> moq_native::ClientConfig {
	let mut config = moq_native::ClientConfig::default();
	config.connect = Some(url.clone());
	config.bind = "127.0.0.1:0".parse().expect("parse client bind");
	config.tls.disable_verify = Some(true);
	config.websocket.enabled = false;

	// A killed relay sends no CONNECTION_CLOSE, so the idle timeout is the only
	// thing that ever tells a client it is gone. The 30s default would put that
	// discovery past every budget here. The keep-alive is an eighth of it, so a
	// busy runner has to stall eight times over before a live session is mistaken
	// for a dead one.
	config.quic.idle_timeout = Some(Duration::from_secs(2));
	config.quic.keep_alive = Some(Duration::from_millis(250));

	// Fast enough to keep a relay bounce inside the drill's budget, paced enough
	// that the loop is still a backoff. `linger` is derived from `timeout`, so
	// the give-up budget also sets how long a broadcast survives the gap.
	config.backoff.initial = Duration::from_millis(50);
	config.backoff.max = Duration::from_millis(200);
	config.backoff.timeout = Duration::from_secs(5);

	config
}

fn client(url: &url::Url) -> moq_native::Client {
	client_config(url).init().expect("client init")
}

/// Append one finished group carrying `payload`.
fn write_group(track: &mut moq_net::track::Producer, payload: &[u8]) {
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, payload)
		.expect("write frame");
	group.finish().expect("finish group");
}

/// Read the next group's first frame, failing the drill on a timeout, a clean
/// finish, or an empty group.
async fn read_group(reader: &mut Reader, what: &str) -> (u64, Vec<u8>) {
	let group = tokio::time::timeout(TIMEOUT, reader.groups.recv_group())
		.await
		.unwrap_or_else(|_| panic!("{what}: no group within {TIMEOUT:?}"))
		.unwrap_or_else(|err| panic!("{what}: track aborted: {err}"))
		.unwrap_or_else(|| panic!("{what}: track finished instead of delivering a group"));
	let sequence = group.sequence;

	let mut group = group;
	let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.unwrap_or_else(|_| panic!("{what}: no frame within {TIMEOUT:?}"))
		.unwrap_or_else(|err| panic!("{what}: group aborted: {err}"))
		.unwrap_or_else(|| panic!("{what}: group finished with no frame"));

	(sequence, frame.payload.to_vec())
}

/// Read until `want` arrives, tolerating groups the disruption truncated.
///
/// A drill that resumes after an outage is asking whether new content flows, not
/// what happened to the groups caught in it, so a group whose frames went with
/// the old session is skipped here. A track-level failure is still fatal: that
/// says the subscription itself did not survive.
async fn read_until(reader: &mut Reader, want: &[u8], what: &str) {
	tokio::time::timeout(TIMEOUT, async {
		loop {
			let mut group = match reader.groups.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => panic!("{what}: the track finished instead of resuming"),
				Err(err) => panic!("{what}: the track aborted: {err}"),
			};
			if let Ok(Some(frame)) = group.read_frame().await
				&& frame.payload == want
			{
				return;
			}
		}
	})
	.await
	.unwrap_or_else(|_| panic!("{what}: {} never arrived", String::from_utf8_lossy(want)));
}

/// Everything a drill holds on the subscribing side of one broadcast.
struct Reader {
	/// The broadcast the session feeds. Outlives the subscription so a drill can
	/// watch what a cancel or a session loss does to it.
	broadcast: moq_net::broadcast::Consumer,
	/// The local mirror of the publisher's track: `latest()` is what arrived,
	/// whatever the subscriber below has bothered to read.
	track: moq_net::track::Consumer,
	groups: moq_net::track::Subscriber,
}

/// Wait for `path` to be announced, then subscribe to [`TRACK`].
async fn subscribe(origin: &moq_net::origin::Consumer, path: &str) -> Reader {
	// `announced_broadcast` parks until the announcement lands; `request_broadcast`
	// would race it and report a live broadcast as unroutable.
	let broadcast = tokio::time::timeout(TIMEOUT, origin.announced_broadcast(path))
		.await
		.expect("announcement timed out")
		.expect("origin closed before announcing");

	let track = broadcast.track(TRACK).expect("track");
	let groups = tokio::time::timeout(TIMEOUT, track.subscribe(None))
		.await
		.expect("subscribe timed out")
		.expect("subscribe rejected");

	Reader {
		broadcast,
		track,
		groups,
	}
}

/// Drill: stall a reader until a backlog builds, then cancel it.
///
/// The fault is a live subscription with unread groups stacked up behind it.
/// Cancelling there has to end cleanly on both counts: the handles that
/// subscription fed are released rather than left parked for a reader that will
/// never return, and the relay keeps serving everyone else.
///
/// What it deliberately does not assert is the publisher going idle. The relay
/// holds an upstream subscription for `TRACK_IDLE_LINGER` (30s in moq-net) after
/// its last local reader leaves, so a viewer who comes back does not pay for a
/// fresh upstream subscribe. Waiting that out would make this the slowest test
/// in the workspace to observe a deliberate delay; the rejoin below is the half
/// of that behavior worth grading.
#[tokio::test]
async fn cancel_under_backpressure_releases_the_reader() {
	let relay = RelayHost::start(None).await;
	let url = relay.url();

	let publisher = Origin::random().produce();
	let mut broadcast = publisher
		.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track(TRACK, None).expect("create track");
	let publish_session = tokio::time::timeout(TIMEOUT, client(&url).with_publisher(&publisher).connect(url.clone()))
		.await
		.expect("publisher connect timed out")
		.expect("publisher connect failed");

	let subscriber = Origin::random().produce();
	let subscribed = subscriber.consume();
	let subscribe_session =
		tokio::time::timeout(TIMEOUT, client(&url).with_subscriber(subscriber).connect(url.clone()))
			.await
			.expect("subscriber connect timed out")
			.expect("subscriber connect failed");

	write_group(&mut track, b"first");
	let mut reader = subscribe(&subscribed, "live").await;
	let (first, payload) = read_group(&mut reader, "before the stall").await;
	assert_eq!(payload, b"first", "delivery is broken before the drill even starts");

	// The stall: the reader stops asking while the publisher runs ahead of it.
	for i in 0..STALL_GROUPS {
		write_group(&mut track, format!("stalled {i}").as_bytes());
		tokio::time::sleep(STALL / STALL_GROUPS as u32).await;
	}

	// Fault activation: the groups reached the subscriber and stacked up behind a
	// reader that never asked for them, so the cancel below lands on a live
	// subscription with a real backlog rather than on an idle one. Nothing blocks
	// a moq publisher (`write_frame` is synchronous, and a reader that falls too
	// far behind is shed rather than waited for), so an unread backlog is what
	// backpressure looks like from here.
	let backlog = tokio::time::timeout(TIMEOUT, async {
		loop {
			match reader.track.latest() {
				Some(latest) if latest >= first + STALL_GROUPS as u64 => return latest - first,
				_ => tokio::time::sleep(Duration::from_millis(10)).await,
			}
		}
	})
	.await
	.unwrap_or_else(|_| {
		panic!(
			"the stalled reader is only {:?} groups behind: nothing was queued, so there is no backlog to cancel under",
			reader.track.latest().map(|latest| latest - first)
		)
	});
	println!("fault activated: cancelling with {backlog} unread groups queued");

	// Nothing has been released yet, so the release below cannot be reporting a
	// state that was already true.
	assert!(
		!reader.broadcast.is_closed(),
		"the subscriber's broadcast closed while it was still reading"
	);

	// The cancel, with the backlog outstanding.
	let cancelled = reader.broadcast.clone();
	drop(reader);
	drop(subscribe_session);

	// Resource release: everything that session was feeding is closed, rather
	// than left parked on a subscription nobody will ever serve again.
	let err = tokio::time::timeout(TIMEOUT, cancelled.closed())
		.await
		.expect("the cancelled subscriber's broadcast never closed");
	println!("resource released: the cancelled broadcast closed with {err}");

	// ...and the relay survived it: a fresh subscriber still gets served, off the
	// upstream subscription the cancel left in place.
	let rejoin = Origin::random().produce();
	let rejoined = rejoin.consume();
	let rejoin_session = tokio::time::timeout(TIMEOUT, client(&url).with_subscriber(rejoin).connect(url.clone()))
		.await
		.expect("rejoin connect timed out")
		.expect("rejoin connect failed");
	let mut rejoined_reader = subscribe(&rejoined, "live").await;
	write_group(&mut track, b"after cancel");
	read_until(&mut rejoined_reader, b"after cancel", "after the cancel").await;

	drop(rejoined_reader);
	drop(rejoin_session);
	drop(publish_session);
}

/// Drill: kill the relay mid-group, then bring it back.
///
/// Two things have to hold. An interrupted track must abort, because a clean
/// finish means "the publisher is done" and silently truncating a live stream
/// into one is the worst possible failure mode. And the reconnect loop has to
/// splice over the gap, so the same handles resume when the relay returns.
#[tokio::test]
async fn relay_killed_mid_group_aborts_then_resumes() {
	let mut relay = RelayHost::start(None).await;
	let port = relay.port;
	let url = relay.url();

	let publisher = Origin::random().produce();
	let mut broadcast = publisher
		.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track(TRACK, None).expect("create track");
	let mut publish_loop = client(&url).publish(publisher.consume()).expect("no connect url");

	let subscriber = Origin::random().produce();
	let subscribed = subscriber.consume();
	let mut subscribe_loop = client(&url).consume(subscriber).expect("no connect url");

	write_group(&mut track, b"before");
	let mut reader = subscribe(&subscribed, "live").await;
	let (_, payload) = read_group(&mut reader, "before the kill").await;
	assert_eq!(payload, b"before", "delivery is broken before the drill even starts");

	// A group that is open, delivered as far as its first frame, and waited on for
	// a second: the kill lands mid-group rather than in the quiet between them.
	// Nothing more is written, so what the reader is waiting for cannot arrive
	// ahead of the kill and turn this into a race.
	let mut open = track.append_group().expect("append group");
	open.write_frame(moq_net::Timestamp::ZERO, b"read".as_ref())
		.expect("write frame");
	let mut open_reader = tokio::time::timeout(TIMEOUT, reader.groups.recv_group())
		.await
		.expect("open group timed out")
		.expect("track aborted")
		.expect("track finished");
	tokio::time::timeout(TIMEOUT, open_reader.read_frame())
		.await
		.expect("open frame timed out")
		.expect("open group aborted")
		.expect("open group finished");

	relay.kill().await;

	// Terminal result: the unread half of the open group fails. `Ok(None)` here
	// would be the relay's death passing for the publisher finishing.
	let err = tokio::time::timeout(TIMEOUT, open_reader.read_frame())
		.await
		.expect("the interrupted group never resolved")
		.expect_err("the interrupted group reported a clean finish");
	println!("fault activated: interrupted group aborted with {err}");

	// Fault activation: both loops noticed they lost the relay.
	expect_status(&mut subscribe_loop, moq_native::Status::Disconnected, "subscriber").await;
	expect_status(&mut publish_loop, moq_native::Status::Disconnected, "publisher").await;

	let relay = RelayHost::start(Some(port)).await;

	expect_status(&mut subscribe_loop, moq_native::Status::Connected, "subscriber").await;
	expect_status(&mut publish_loop, moq_native::Status::Connected, "publisher").await;

	// Resumed delivery, on the handles that survived the outage: the broadcast
	// lingered across the gap and the subscription spliced onto the new session.
	open.finish().expect("finish the interrupted group");
	write_group(&mut track, b"after");
	read_until(&mut reader, b"after", "after the restart").await;

	drop(reader);
	drop(subscribe_loop);
	drop(publish_loop);
	drop(relay);
}

/// Wait for a reconnect loop to report `want`, failing with what it said instead.
async fn expect_status(reconnect: &mut moq_native::Reconnect, want: moq_native::Status, who: &str) {
	loop {
		let status = tokio::time::timeout(TIMEOUT, reconnect.status())
			.await
			.unwrap_or_else(|_| panic!("{who}: no status change within {TIMEOUT:?}, wanted {want:?}"))
			.unwrap_or_else(|err| panic!("{who}: reconnect loop gave up: {err}"));
		if status == want {
			return;
		}
	}
}

/// Drill: interrupt a publisher, then republish the same name.
///
/// A crashed publisher must withdraw its broadcast rather than leave a name
/// announced that nothing serves, and the replacement has to be new content: a
/// subscriber that re-consumes the same name after a republish gets what the
/// new publisher is sending, never the previous one's cache.
#[tokio::test]
async fn interrupted_publisher_republishes_new_content() {
	let relay = RelayHost::start(None).await;
	let url = relay.url();

	let subscriber = Origin::random().produce();
	let subscribed = subscriber.consume();
	let subscribe_session =
		tokio::time::timeout(TIMEOUT, client(&url).with_subscriber(subscriber).connect(url.clone()))
			.await
			.expect("subscriber connect timed out")
			.expect("subscriber connect failed");
	let mut announced = subscribed.announced();

	let first = Origin::random().produce();
	let mut broadcast = first
		.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track(TRACK, None).expect("create track");
	let first_session = tokio::time::timeout(TIMEOUT, client(&url).with_publisher(&first).connect(url.clone()))
		.await
		.expect("publisher connect timed out")
		.expect("publisher connect failed");

	write_group(&mut track, b"original");
	expect_announce(&mut announced, "live", true, "the first publisher").await;
	let mut reader = subscribe(&subscribed, "live").await;
	let (_, payload) = read_group(&mut reader, "before the interrupt").await;
	assert_eq!(payload, b"original", "delivery is broken before the drill even starts");

	// The interrupt: everything the publisher owned disappears at once, with no
	// finish and no unannounce, which is what a crashed publisher looks like. The
	// subscriber's handles on it go too, so nothing local keeps the dead
	// broadcast alive for the assertions below.
	drop(reader);
	drop(track);
	drop(broadcast);
	drop(first);
	drop(first_session);

	// Terminal result: the name stops being announced. The relay lingers a
	// broadcast whose publisher vanished, so this is also the proof that the
	// linger window ends rather than parking subscribers forever.
	expect_announce(&mut announced, "live", false, "the interrupted publisher").await;
	println!("fault activated: the interrupted publisher's broadcast was withdrawn");

	// Restore: the same name, a new publisher, different content.
	let second = Origin::random().produce();
	let mut broadcast = second
		.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
		.expect("re-create broadcast");
	let mut track = broadcast.create_track(TRACK, None).expect("re-create track");
	let second_session = tokio::time::timeout(TIMEOUT, client(&url).with_publisher(&second).connect(url.clone()))
		.await
		.expect("republisher connect timed out")
		.expect("republisher connect failed");

	write_group(&mut track, b"replacement");
	expect_announce(&mut announced, "live", true, "the republisher").await;

	let mut reader = subscribe(&subscribed, "live").await;
	let (_, payload) = read_group(&mut reader, "after the republish").await;
	assert_eq!(
		payload, b"replacement",
		"the republished name served the dead publisher's content"
	);

	drop(reader);
	drop(track);
	drop(broadcast);
	drop(second_session);
	drop(subscribe_session);
}

/// Wait for `path` to be announced (`want`) or unannounced (`!want`).
async fn expect_announce(announced: &mut moq_net::announce::Consumer, path: &str, want: bool, who: &str) {
	loop {
		let update = tokio::time::timeout(TIMEOUT, announced.next())
			.await
			.unwrap_or_else(|_| panic!("{who}: no announcement change within {TIMEOUT:?}"))
			.unwrap_or_else(|| panic!("{who}: the announcement stream closed"));
		if update.path.as_str() == path && update.broadcast.is_some() == want {
			return;
		}
	}
}

/// Negative control: with nothing publishing, the drills' delivery assertions
/// have to fail.
///
/// Every drill above proves something by reading a frame. This one runs the same
/// harness with no publisher and requires that no announcement and no broadcast
/// ever arrive, so "the frame showed up" cannot be a harness artifact.
#[tokio::test]
async fn no_publisher_never_delivers() {
	let relay = RelayHost::start(None).await;
	let url = relay.url();

	let subscriber = Origin::random().produce();
	let subscribed = subscriber.consume();
	let session = tokio::time::timeout(TIMEOUT, client(&url).with_subscriber(subscriber).connect(url.clone()))
		.await
		.expect("subscriber connect timed out")
		.expect("subscriber connect failed");

	// Short on purpose: this is the one wait that must expire, so it is the one
	// wait that decides how long the suite spends proving a negative.
	let quiet = Duration::from_secs(2);

	let mut announced = subscribed.announced();
	if let Ok(update) = tokio::time::timeout(quiet, announced.next()).await {
		let path = update.map(|update| update.path.to_string());
		panic!("the announcement stream reported {path:?} with no publisher");
	}

	assert!(
		tokio::time::timeout(quiet, subscribed.announced_broadcast("live"))
			.await
			.is_err(),
		"a broadcast resolved with no publisher"
	);

	drop(session);
	drop(relay);
}

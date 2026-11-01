//! Real relay routing keeps held catalog/media identities while fresh readers follow replacements.

use std::time::Duration;

use kio::clock::Instant;

use hang::moq_net::{self, Epoch, Timestamp, announce, origin, track};
use moq_mux::catalog::Stream;

const LITE_07: &str = "moq-lite-07-wip";

/// A loopback round trip, charged at QUIC's timer granularity (RFC 9002's kGranularity).
const ROUND_TRIP: Duration = Duration::from_millis(1);

/// How long an announcement-following reader may take to see a replacement or a fallback: the
/// relay's and the reader's origins may each hold the route change for their update hold, and
/// the announcement crosses both links in one round trip. The idle timeout never enters it.
fn routing() -> Duration {
	origin::Config::default()
		.update_hold
		.saturating_mul(2)
		.saturating_add(ROUND_TRIP)
}

struct Relay {
	url: url::Url,
	running: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Drop for Relay {
	fn drop(&mut self) {
		self.running.abort();
	}
}

async fn relay() -> Relay {
	let _ = moq_tokio::crypto::install_default();
	let mut config = moq_relay::Config::default();
	config.drain_timeout = Some(Duration::ZERO.into());
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.generate = vec!["localhost".into()];
	config.listen.version = std::iter::once(LITE_07.parse().unwrap())
		.chain(moq_net::Versions::all().iter().copied())
		.collect();
	config.auth.public = vec![moq_auth::Pattern::all()];
	let relay = moq_relay::Relay::load(config).await.unwrap();
	let url = format!("https://{}/", relay.quic_addr().unwrap()).parse().unwrap();
	let ready = relay.ready();
	let running = tokio::spawn(relay.run());
	ready.wait().await.unwrap();
	Relay { url, running }
}

fn client(version: &str) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config.version = vec![version.parse().unwrap()];
	config.init(Default::default()).unwrap().with_reconnect(false)
}

fn write(track: &track::Producer, tag: &str) {
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, tag.as_bytes().to_vec()).unwrap();
	group.finish().unwrap();
}

async fn read(subscription: &mut track::Subscriber) -> String {
	let mut group = subscription.recv_group().await.unwrap().unwrap();
	let frame = group.read_frame().await.unwrap().unwrap();
	assert!(
		group.read_frame().await.unwrap().is_none(),
		"one finished frame per group"
	);
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

struct Published {
	catalog: moq_mux::catalog::Producer,
	_catalog_broadcast: origin::Publication,
	sibling: origin::Publication,
	media: track::Producer,
}

fn publish(origin: &origin::Producer, epoch: &Epoch, tag: &str) -> Published {
	let route = origin::Route::default().with_epoch(epoch.clone());
	let broadcast = origin.publish("live/catalog.hang", route.clone()).unwrap();
	let sibling = origin.publish("live/media", route).unwrap();
	let media = sibling
		.create_track(
			"audio",
			track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.unwrap();
	write(&media, &format!("{tag}:0"));
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
	catalog
		.mutate(|catalog| {
			let mut audio = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
			audio.broadcast = Some(moq_net::path::Relative::new("./media").unwrap());
			audio.epoch = Some(epoch.clone());
			catalog.audio.renditions.insert("audio".into(), audio);
		})
		.unwrap();
	Published {
		catalog,
		_catalog_broadcast: broadcast,
		sibling,
		media,
	}
}

/// Finish a route change within [`routing`] of `started`.
async fn within<T>(started: Instant, change: impl Future<Output = T>) -> T {
	let deadline = started + routing();
	kio::clock::timeout_at(deadline, change)
		.await
		.unwrap_or_else(|_| panic!("route change took longer than {:?}", routing()))
}

/// Ignore same-instance metadata updates, but keep the actual replacement representation.
async fn changed(follow: &mut announce::Consumer) -> bool {
	loop {
		match follow.next().await.unwrap() {
			announce::Event::Restart(_) => return true,
			announce::Event::End(_) => {
				assert!(matches!(follow.next().await, Some(announce::Event::Start(_))));
				return false;
			}
			announce::Event::Update(_) => {}
			other => panic!("expected replacement, got {other:?}"),
		}
	}
}

async fn replacement(version: &str) {
	let relay = relay().await;
	let (a, b) = (Epoch::mint(), Epoch::mint());
	assert!(b > a);
	let first_origin = moq_tokio::origin::spawn();
	let mut first = publish(&first_origin, &a, "A");
	let first_client = client(LITE_07).with_publisher(&first_origin);
	let _first_connection = first_client
		.clone()
		.connect(relay.url.clone())
		.established()
		.await
		.unwrap();
	let viewer_origin = moq_tokio::origin::spawn();
	let viewer = viewer_origin.consume();
	let watch = |path| {
		let path = moq_net::Patterns::from(moq_net::Pattern::literal(path).unwrap());
		viewer.scope("", &path).unwrap().announced()
	};
	let mut catalog_follow = watch("live/catalog.hang");
	let mut media_follow = watch("live/media");
	let viewer_client = client(version).with_subscriber(viewer_origin);
	let _viewer_connection = viewer_client
		.clone()
		.connect(relay.url.clone())
		.established()
		.await
		.unwrap();
	assert!(matches!(catalog_follow.next().await, Some(announce::Event::Start(_))));
	assert!(matches!(media_follow.next().await, Some(announce::Event::Start(_))));

	let held = viewer.request("live/catalog.hang", None).await.unwrap();
	let old = moq_mux::Source::new(viewer.clone(), held);
	let mut catalog = old.catalog::<()>(moq_mux::catalog::CatalogFormat::Hang).await.unwrap();
	let snapshot = catalog.next().await.unwrap().unwrap();
	let reference = &snapshot.audio.renditions["audio"];
	let pinned = old
		.resolve(reference.broadcast.as_ref(), reference.epoch.as_ref())
		.await
		.unwrap();
	let cold = pinned.track("audio").unwrap();
	let mut subscription = cold.subscribe(None).await.unwrap();
	assert_eq!(read(&mut subscription).await, "A:0");
	// A lazy handle made before the first lookup has no cached instance to fall
	// back to after retirement. This second handle observes the indexed track.
	let media = pinned.track("audio").unwrap();
	let mut cached = media
		.fetch_group(0, None)
		.await
		.expect("A:0 is fetchable before replacement");
	assert_eq!(cached.read_frame().await.unwrap().unwrap().payload.as_ref(), b"A:0");
	assert!(cached.read_frame().await.unwrap().is_none());

	// B claims both paths while A's session, catalog, and media producer remain open.
	let second_origin = moq_tokio::origin::spawn();
	let second_client = client(LITE_07).with_publisher(&second_origin);
	let second_connection = second_client
		.clone()
		.connect(relay.url.clone())
		.established()
		.await
		.unwrap();
	let started = Instant::now();
	let second = publish(&second_origin, &b, "B");
	write(&second.media, "B:1");
	write(&second.media, "B:2");
	let _late = second
		.sibling
		.create_track("late", track::Info::default().with_priority(17))
		.unwrap();
	let (catalog_restart, media_restart) = within(started, async {
		(changed(&mut catalog_follow).await, changed(&mut media_follow).await)
	})
	.await;
	assert_eq!(catalog_restart, version != "moq-lite-06");
	assert_eq!(media_restart, catalog_restart);
	println!(
		"{version}: relay replacement observed in {:?} of {:?} (publisher already connected)",
		started.elapsed(),
		routing()
	);

	// Later catalog snapshots stay on A and resolving again keeps A's sibling pin.
	first
		.catalog
		.mutate(|catalog| catalog.audio.renditions.get_mut("audio").unwrap().bitrate = Some(64_000))
		.unwrap();
	let later = catalog.next().await.unwrap().unwrap();
	assert_eq!(later.audio.renditions["audio"].bitrate, Some(64_000));
	assert!(
		old.resolve(reference.broadcast.as_ref(), None)
			.await
			.unwrap()
			.is_clone(&pinned)
	);
	write(&first.media, "A:1");
	assert_eq!(read(&mut subscription).await, "A:1");

	// FETCH cannot fill A's missing group from B, even though B has that group cached.
	assert!(cold.fetch_group(2, None).await.is_err());
	assert!(media.fetch_group(2, None).await.is_err());
	write(&first.media, "A:2");
	assert_eq!(
		read(&mut subscription).await,
		"A:2",
		"a refused FETCH leaves A's held reader live"
	);
	let mut cached = media.fetch_group(0, None).await.unwrap();
	assert_eq!(cached.read_frame().await.unwrap().unwrap().payload.as_ref(), b"A:0");
	if let Ok(late) = pinned.track("late")
		&& let Ok(info) = late.info().await
	{
		assert_ne!(info.priority, 17, "retired handle resolved B's metadata");
	}

	let subscribed = match pinned.track("late") {
		Ok(late) => late.subscribe(None).await.map(|_| ()),
		Err(err) => Err(err),
	};
	assert!(subscribed.is_err(), "retired handle subscribed to B's late track");

	// Fresh catalog handles start fresh pins instead of sharing the old Source's cache.
	let fresh = moq_mux::Source::new(viewer.clone(), viewer.request("live/catalog.hang", None).await.unwrap());
	let mut fresh_catalog = fresh
		.catalog::<()>(moq_mux::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	let snapshot = fresh_catalog.next().await.unwrap().unwrap();
	let reference = &snapshot.audio.renditions["audio"];
	assert_eq!(reference.epoch.as_ref(), Some(&b));
	let new = fresh
		.resolve(reference.broadcast.as_ref(), reference.epoch.as_ref())
		.await
		.unwrap();
	assert!(!new.is_clone(&pinned));
	let mut new_subscription = new.track("audio").unwrap().subscribe(None).await.unwrap();
	let new_frame = read(&mut new_subscription).await;
	assert!(new_frame.starts_with("B:"), "fresh subscription read {new_frame}");

	// A still serves its held reader. Withdrawing B reveals A to fresh readers too.
	let started = Instant::now();
	second_connection.abort(moq_net::Error::Cancel);
	drop(second);
	drop(second_origin);
	within(started, async {
		changed(&mut catalog_follow).await;
		changed(&mut media_follow).await;
	})
	.await;
	let returned = viewer.request("live/media", None).await.unwrap();
	let mut returned = returned.track("audio").unwrap().subscribe(None).await.unwrap();
	write(&first.media, "A:3");
	let returned_frame = read(&mut returned).await;
	assert!(returned_frame.starts_with("A:"), "fallback read {returned_frame}");
	assert_eq!(read(&mut subscription).await, "A:3");
	println!(
		"{version}: relay fallback observed in {:?} of {:?}",
		started.elapsed(),
		routing()
	);
}

macro_rules! versions {
	($($name:ident: $version:literal),+ $(,)?) => {$(
		#[tokio::test]
		async fn $name() {
			replacement($version).await;
		}
	)+};
}

versions! {
	lite07: "moq-lite-07-wip",
	lite06: "moq-lite-06",
	ietf22: "moq-transport-22",
}

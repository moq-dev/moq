//! Real relay routing keeps held catalog/media identities while fresh readers follow replacements.
//!
//! Wait for route events over real sockets. The origin's controlled-time tests cover update holds;
//! elapsed host time here also includes scheduling and cannot bound that behavior.

use std::time::Duration;

use moq_mux::catalog::Stream;
use moq_net::{Instance, Timestamp, origin, track};

const LITE_07: &str = "moq-lite-07-wip";

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
	moq_tokio::crypto::install_default().unwrap();
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
	let start = track.largest().map_or(0, |largest| largest.group + 1);
	let mut group = track.append_group(start).unwrap();
	group
		.write_frame(Timestamp::from_millis(start).unwrap(), tag.as_bytes().to_vec())
		.unwrap();
	group.close().unwrap();
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
	_catalog_broadcast: origin::Route,
	sibling: origin::Route,
	media: track::Producer,
}

fn publish(origin: &origin::Producer, instance: &Instance, tag: &str) -> Published {
	let options = || origin::RouteOptions::default().with_instance(instance.clone(), 0);
	let broadcast = origin.route("live/catalog.hang", options()).unwrap();
	let sibling = origin.route("live/media", options()).unwrap();
	// Media's max age, so its superseded groups stay fetchable.
	let media = sibling
		.broadcast()
		.create_track(
			"audio",
			track::Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(hang::container::MAX_AGE),
		)
		.unwrap();
	write(&media, &format!("{tag}:0"));
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
	catalog
		.mutate(|catalog| {
			let mut audio = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
			audio.broadcast = Some(moq_net::path::Relative::new("./media").unwrap());
			audio.instance = Some(instance.clone());
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

/// Ignore same-instance metadata updates, but keep the actual replacement representation.
async fn changed(follow: &mut origin::Routes) -> bool {
	loop {
		match follow.next().await.unwrap() {
			origin::RouteEvent {
				kind: origin::RouteKind::Restart,
				..
			} => return true,
			origin::RouteEvent {
				kind: origin::RouteKind::End | origin::RouteKind::Offline,
				..
			} => {
				assert!(matches!(
					follow.next().await,
					Some(origin::RouteEvent {
						kind: origin::RouteKind::Start,
						..
					})
				));
				return false;
			}
			origin::RouteEvent {
				kind: origin::RouteKind::Update,
				..
			} => {}
			other => panic!("expected replacement, got {other:?}"),
		}
	}
}

async fn replacement(version: &str) {
	let relay = relay().await;
	let (a, b) = (Instance::random(), Instance::random());
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
		viewer.scope("", &path).unwrap().routes()
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
	assert!(matches!(
		catalog_follow.next().await,
		Some(origin::RouteEvent {
			kind: origin::RouteKind::Start,
			..
		})
	));
	assert!(matches!(
		media_follow.next().await,
		Some(origin::RouteEvent {
			kind: origin::RouteKind::Start,
			..
		})
	));

	let held = viewer.request("live/catalog.hang", None).await.unwrap();
	let old = moq_mux::Source::new(viewer.clone(), held);
	let mut catalog = old.catalog::<()>(moq_mux::catalog::CatalogFormat::Hang).await.unwrap();
	let snapshot = catalog.next().await.unwrap().unwrap();
	let reference = &snapshot.audio.renditions["audio"];
	let pinned = old
		.resolve(reference.broadcast.as_ref(), reference.instance.as_ref())
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
	let second = publish(&second_origin, &b, "B");
	write(&second.media, "B:1");
	write(&second.media, "B:2");
	let _late = second
		.sibling
		.broadcast()
		.create_track("late", track::Info::default().with_priority(17))
		.unwrap();
	let catalog_restart = changed(&mut catalog_follow).await;
	let media_restart = changed(&mut media_follow).await;
	assert_eq!(
		catalog_restart,
		version == LITE_07,
		"only lite-07 carries the instance, which restarts in place"
	);
	assert_eq!(media_restart, catalog_restart);

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
	assert_eq!(reference.instance.as_ref(), Some(&b));
	let new = fresh
		.resolve(reference.broadcast.as_ref(), reference.instance.as_ref())
		.await
		.unwrap();
	assert!(!new.is_clone(&pinned));
	let mut new_subscription = new.track("audio").unwrap().subscribe(None).await.unwrap();
	let new_frame = read(&mut new_subscription).await;
	assert!(new_frame.starts_with("B:"), "fresh subscription read {new_frame}");

	// A still serves its held reader. Withdrawing B reveals A to fresh readers too.
	second_connection.abort(moq_net::Error::Cancel);
	drop(second);
	drop(second_origin);
	changed(&mut catalog_follow).await;
	changed(&mut media_follow).await;
	let returned = viewer.request("live/media", None).await.unwrap();
	let mut returned = returned.track("audio").unwrap().subscribe(None).await.unwrap();
	write(&first.media, "A:3");
	let returned_frame = read(&mut returned).await;
	assert!(returned_frame.starts_with("A:"), "fallback read {returned_frame}");
	assert_eq!(read(&mut subscription).await, "A:3");
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

/// A worker claiming `live`, answering each request with a job's exact route: the catalog,
/// which names `./media` under the job's instance, and the media it references.
struct Worker {
	claim: origin::Claim,
	jobs: std::sync::Arc<std::sync::Mutex<Vec<Job>>>,
}

/// One answered request, held so its route stays advertised.
struct Job {
	path: String,
	route: origin::Route,
	_catalog: Option<moq_mux::catalog::Producer>,
	media: Option<track::Producer>,
}

impl Worker {
	fn new(origin: &origin::Producer) -> Self {
		let claim = origin.claim("live", origin::RouteOptions::default()).unwrap();
		let jobs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
		let (handler, answered) = (claim.clone(), jobs.clone());
		tokio::spawn(async move {
			while let Some(request) = handler.requested().await {
				let path = request.path().to_string();
				let route = request.route(origin::RouteOptions::hidden()).unwrap();
				// Hidden, the job reports the instance it will announce under: the claim's.
				let instance = route.instance();
				let (catalog, media) = match path.as_str() {
					"live/catalog.hang" => {
						let mut catalog =
							moq_mux::catalog::Producer::new(&mut route.broadcast().clone(), Default::default())
								.unwrap();
						catalog
							.mutate(|catalog| {
								let mut audio =
									hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
								audio.broadcast = Some(moq_net::path::Relative::new("./media").unwrap());
								audio.instance = Some(instance);
								catalog.audio.renditions.insert("audio".into(), audio);
							})
							.unwrap();
						(Some(catalog), None)
					}
					_ => {
						let media = route
							.broadcast()
							.create_track(
								"audio",
								track::Info::default().with_timescale(moq_net::Timescale::MILLI),
							)
							.unwrap();
						write(&media, "job:0");
						(None, Some(media))
					}
				};
				route.announce().unwrap();
				answered.lock().unwrap().push(Job {
					path,
					route,
					_catalog: catalog,
					media,
				});
			}
		});
		Self { claim, jobs }
	}

	fn answered(&self) -> Vec<String> {
		self.jobs.lock().unwrap().iter().map(|job| job.path.clone()).collect()
	}
}

/// A player's first catalog read through a worker's claim continues on the job's exact route
/// over a real lite-07 relay, and the catalog's pinned media resolves through the claim to the
/// media job under the same instance. Once that media job ends the claim moves on, so the
/// held pin is refused rather than answered by a restarted job.
#[tokio::test]
async fn a_player_reads_through_a_claim_on_its_first_request() {
	let relay = relay().await;
	let worker_origin = moq_tokio::origin::spawn();
	let worker = Worker::new(&worker_origin);
	let lent = worker.claim.instance();
	let _worker_connection = client(LITE_07)
		.with_publisher(&worker_origin)
		.connect(relay.url.clone())
		.established()
		.await
		.unwrap();
	let viewer_origin = moq_tokio::origin::spawn();
	let viewer = viewer_origin.consume();
	let mut routes = viewer.routes();
	let _viewer_connection = client(LITE_07)
		.with_subscriber(viewer_origin)
		.connect(relay.url.clone())
		.established()
		.await
		.unwrap();
	assert!(matches!(
		routes.next().await,
		Some(origin::RouteEvent {
			kind: origin::RouteKind::Start,
			..
		})
	));

	let held = viewer.request("live/catalog.hang", None).await.unwrap();
	let source = moq_mux::Source::new(viewer.clone(), held);
	let mut catalog = source
		.catalog::<()>(moq_mux::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	let snapshot = catalog.next().await.unwrap().unwrap();
	let reference = &snapshot.audio.renditions["audio"];
	assert_eq!(
		reference.instance.as_ref(),
		Some(&lent),
		"the job serves the claim's instance"
	);
	let pinned = source
		.resolve(reference.broadcast.as_ref(), reference.instance.as_ref())
		.await
		.unwrap();
	let mut subscription = pinned.track("audio").unwrap().subscribe(None).await.unwrap();
	assert_eq!(read(&mut subscription).await, "job:0");
	assert_eq!(worker.answered(), ["live/catalog.hang", "live/media"]);

	// The media job ends: the claim moves on, and nothing answers the old pin again.
	let media = {
		let mut jobs = worker.jobs.lock().unwrap();
		let index = jobs.iter().position(|job| job.media.is_some()).unwrap();
		jobs.remove(index)
	};
	media.route.broadcast().close();
	drop(media);
	assert_ne!(worker.claim.instance(), lent);
	loop {
		match routes.next().await.unwrap() {
			origin::RouteEvent {
				kind: origin::RouteKind::End | origin::RouteKind::Offline,
				announce: end,
			} if end.prefix.as_str() == "live/media" => break,
			_ => {}
		}
	}
	assert!(
		viewer.request("live/media", lent).await.is_err(),
		"a request pinned to the ended job resolved"
	);
	assert_eq!(
		worker.answered(),
		["live/catalog.hang"],
		"a pinned request woke the worker"
	);
}

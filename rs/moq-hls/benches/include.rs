//! Master rendering and one include lookup, swept over includes x renditions.
//! The catalogs have no media, so setup also verifies that listing does not need an encode.
//!
//! Run with `cargo bench -p moq-hls --features server --bench include`.

use std::fmt;
use std::time::Duration;

use hang::catalog::{Include, VideoCodec, VideoConfig};
use moq_hls::{Server, export::Route};
use moq_mux::catalog::hang::Catalog;

fn publish_catalog(
	broadcast: &moq_net::broadcast::Producer,
	catalog: &Catalog,
) -> moq_json::snapshot::Producer<Catalog> {
	let track = broadcast
		.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
		.unwrap();
	let mut producer = moq_json::snapshot::Producer::new(track, Default::default());
	producer.update(catalog, moq_net::Timestamp::ZERO).unwrap();
	producer
}

/// Included catalogs and the video renditions each holds, named `<count>c_<renditions>r`.
#[derive(Clone, Copy)]
struct Case {
	count: usize,
	renditions: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}c_{}r", self.count, self.renditions)
	}
}

fn cases() -> Vec<Case> {
	[1, 8, 64]
		.into_iter()
		.flat_map(|count| [1, 4, 16].map(|renditions| Case { count, renditions }))
		.collect()
}

/// A server listing a root catalog that includes every published catalog, and a route that
/// looks up a rendition missing from the first include.
struct Fixture {
	server: Server,
	broadcaster: moq_hls::export::Broadcaster,
	route: Route,
	_publications: Vec<moq_net::origin::Route>,
	_catalogs: Vec<moq_json::snapshot::Producer<Catalog>>,
	_origin: moq_net::origin::Producer,
	// Declared last so it drops after the tasks it drives.
	runtime: tokio::runtime::Runtime,
}

fn fixture(case: Case) -> Fixture {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let (server, broadcaster, route, publications, catalogs, origin) = runtime.block_on(async {
		let origin = moq_tokio::origin::spawn();
		let mut root = Catalog::default();
		root.sync.interval = Some(Duration::from_secs(2));
		let mut publications = Vec::new();
		let mut catalogs = Vec::new();
		let mut first = None;
		for index in 0..case.count {
			let id = format!("c{index}");
			let identity = moq_net::Identity::random();
			first.get_or_insert(identity.clone());
			let route = origin
				.route(
					format!("room/{id}"),
					moq_net::origin::RouteOptions::hidden().with_identity(identity, 0),
				)
				.unwrap();
			let mut catalog = Catalog::default();
			catalog.sync = root.sync;
			for rendition in 0..case.renditions {
				let mut video = VideoConfig::new(VideoCodec::VP8);
				video.coded_width = Some(320);
				video.coded_height = Some(240);
				catalog.video.renditions.insert(format!("v{rendition}"), video);
			}
			catalogs.push(publish_catalog(route.broadcast(), &catalog));
			route.announce().unwrap();
			publications.push(route);
			root.include.insert(
				id.clone(),
				Include::new(moq_net::path::Relative::new(&format!("./{id}")).unwrap()),
			);
		}
		let parent = moq_net::Identity::random();
		let route = origin
			.route(
				"room/live",
				moq_net::origin::RouteOptions::hidden().with_identity(parent.clone(), 0),
			)
			.unwrap();
		catalogs.push(publish_catalog(route.broadcast(), &root));
		route.announce().unwrap();
		publications.push(route);
		let server = Server::new(origin.consume(), Default::default()).unwrap();
		let broadcaster = server.broadcaster("room/live").await.unwrap();
		kio::clock::timeout(Duration::from_secs(5), async {
			while broadcaster.master_playlist(None).matches("#EXT-X-STREAM-INF:").count()
				!= case.count * case.renditions
			{
				kio::clock::sleep(Duration::from_millis(1)).await;
			}
		})
		.await
		.expect("every catalog is listed without media");
		let route = Route::parse(&format!(
			"/room/live/@{parent}/include/c0/video/missing/@{}/media.m3u8",
			first.unwrap()
		))
		.unwrap();
		(server, broadcaster, route, publications, catalogs, origin)
	});
	Fixture {
		server,
		broadcaster,
		route,
		_publications: publications,
		_catalogs: catalogs,
		_origin: origin,
		runtime,
	}
}

mod hls_include {
	use swole::bench;

	use super::{Fixture, cases, fixture};

	#[bench(args = cases(), setup = fixture)]
	fn master(fixture: &Fixture) -> String {
		fixture.broadcaster.master_playlist(None)
	}

	#[bench(args = cases(), setup = fixture)]
	fn lookup(fixture: &Fixture) -> axum::response::Response {
		fixture.runtime.block_on(fixture.server.respond(&fixture.route, None))
	}
}

fn main() {
	swole::main();
}

//! Master rendering and one contribution lookup, swept over contributions x renditions.
//! The catalogs have no media, so setup also verifies that listing does not need an encode.

use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use hang::catalog::{Contribution, Join, VideoCodec, VideoConfig};
use moq_hls::{Server, export::Route};
use moq_mux::catalog::hang::Catalog;

fn publish_catalog(
	broadcast: &moq_net::origin::Publication,
	catalog: &Catalog,
) -> moq_json::snapshot::Producer<Catalog> {
	let track = broadcast
		.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
		.unwrap();
	let mut producer = moq_json::snapshot::Producer::new(track, Default::default());
	producer
		.update(moq_net::Timed::from(catalog).at(moq_net::Timestamp::ZERO))
		.unwrap();
	producer
}

fn contributions(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("hls/contributions");
	for count in [1, 8, 64] {
		for renditions in [1, 4, 16] {
			let (server, broadcaster, route, _publications, _catalogs, _origin) = runtime.block_on(async {
				let origin = moq_tokio::origin::spawn();
				let mut root = Catalog::default();
				root.media.join = Join::new(Duration::from_secs(2));
				let mut publications = Vec::new();
				let mut catalogs = Vec::new();
				let mut first = None;
				for index in 0..count {
					let id = format!("c{index}");
					let instance = moq_net::Instance::random();
					first.get_or_insert(instance.clone());
					let broadcast = origin.create_broadcast(format!("room/{id}")).unwrap();
					let mut catalog = Catalog::default();
					catalog.media = root.media;
					for rendition in 0..renditions {
						let mut video = VideoConfig::new(VideoCodec::VP8);
						video.coded_width = Some(320);
						video.coded_height = Some(240);
						catalog.video.renditions.insert(format!("v{rendition}"), video);
					}
					catalogs.push(publish_catalog(&broadcast, &catalog));
					broadcast
						.announce(moq_net::origin::Route::default().with_instance(instance, 0))
						.unwrap();
					publications.push(broadcast);
					root.contributions.insert(
						id.clone(),
						Contribution::new(moq_net::path::Relative::new(&format!("./{id}")).unwrap()),
					);
				}
				let parent = moq_net::Instance::random();
				let broadcast = origin.create_broadcast("room/live").unwrap();
				catalogs.push(publish_catalog(&broadcast, &root));
				broadcast
					.announce(moq_net::origin::Route::default().with_instance(parent.clone(), 0))
					.unwrap();
				publications.push(broadcast);
				let server = Server::new(origin.consume(), Default::default()).unwrap();
				let broadcaster = server.broadcaster("room/live").await.unwrap();
				kio::clock::timeout(Duration::from_secs(5), async {
					while broadcaster.master_playlist(None).matches("#EXT-X-STREAM-INF:").count() != count * renditions
					{
						kio::clock::sleep(Duration::from_millis(1)).await;
					}
				})
				.await
				.expect("every catalog is listed without media");
				let route = Route::parse(&format!(
					"/room/live/@{parent}/contribution/c0/video/missing/@{}/media.m3u8",
					first.unwrap()
				))
				.unwrap();
				(server, broadcaster, route, publications, catalogs, origin)
			});
			let shape = format!("{count}c_{renditions}r");
			group.bench_function(BenchmarkId::new("master", &shape), |b| {
				b.iter(|| black_box(broadcaster.master_playlist(None)))
			});
			group.bench_function(BenchmarkId::new("lookup", &shape), |b| {
				b.iter(|| black_box(runtime.block_on(server.respond(&route, None))))
			});
			// Let cancelled driver tasks release their subscriptions before the next shape.
			drop((server, broadcaster, _publications, _catalogs, _origin));
			runtime.block_on(tokio::task::yield_now());
		}
	}
	group.finish();
}

criterion_group!(benches, contributions);
criterion_main!(benches);

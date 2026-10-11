//! Container export cost swept over independently varied tracks and queued frames.
use std::time::Duration;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use hang::catalog::{AAC, AudioConfig};
use moq_mux::catalog::hang::Container;
use moq_mux::container::{Frame, Kind};
use moq_net::Timestamp;

async fn export(format: &str, tracks: usize, frames: usize) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let stream = moq_mux::catalog::Consumer::Hang(catalog.consume().unwrap());
	let mut publishers = Vec::new();
	for index in 0..tracks {
		let track = broadcast
			.create_track(
				format!("audio{index}"),
				catalog.track_info(hang::catalog::PRIORITY.audio),
			)
			.unwrap();
		let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
		config.description = Some(Bytes::from_static(&[0x12, 0x10]));
		publishers.push(catalog.audio(track, Container::Legacy(Kind::Audio), config).unwrap());
	}
	for track in &mut publishers {
		for index in 0..frames {
			track
				.write(Frame {
					timestamp: Timestamp::from_millis(index as u64 * 20).unwrap(),
					duration: None,
					payload: Bytes::from_static(b"audio-frame"),
					keyframe: index == 0,
				})
				.unwrap();
		}
		track.finish().unwrap();
	}
	catalog.finish().unwrap();
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let dynamic = origin.dynamic("", Default::default()).unwrap();
	let served = consumer.clone();
	let requests = tokio::spawn(async move {
		while let Ok(request) = dynamic.requested_broadcast().await {
			request.accept(served.clone());
		}
	});
	let source = moq_mux::Source::new(origin.consume(), "bench");
	let mut chunks = 0;
	if format == "flv" {
		let mut export = moq_mux::container::flv::Export::new(source, stream).with_multitrack(true);
		while let Some(chunk) = export.next().await.unwrap() {
			std::hint::black_box(chunk);
			chunks += 1;
		}
	} else {
		let mut export = moq_mux::container::mkv::Export::new(source, stream).with_fragment_duration(Duration::ZERO);
		while let Some(chunk) = export.next().await.unwrap() {
			std::hint::black_box(chunk);
			chunks += 1;
		}
	}
	assert_eq!(chunks, 1 + tracks * frames);
	requests.abort();
	driver.abort();
	let _ = requests.await;
	let _ = driver.await;
}

fn bench_export(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("container-export");
	group.sample_size(10);
	group.measurement_time(Duration::from_secs(1));
	for format in ["flv", "mkv"] {
		for tracks in [1, 4, 16] {
			for frames in [1, 16, 64] {
				group.throughput(Throughput::Elements((tracks * frames) as u64));
				group.bench_function(BenchmarkId::new(format, format!("{tracks}x{frames}")), |b| {
					b.iter(|| runtime.block_on(export(format, tracks, frames)));
				});
			}
		}
	}
	group.finish();
}

criterion_group!(benches, bench_export);
criterion_main!(benches);

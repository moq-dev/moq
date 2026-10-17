//! Carrying one frame through an encrypted broadcast, swept over the tracks it carries.
//!
//! [`moq_mux::e2ee::Producer`] re-polls only the carried track that woke, so one frame on one
//! track should cost the same however many tracks the broadcast carries. A slope here means a
//! wake started touching every track.
//!
//! Run with `cargo bench -p moq-mux --bench e2ee`.

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_mux::catalog::hang::Container;
use moq_mux::container::Kind;

/// Tracks carried: one rendition, a ladder, a large ladder.
const TRACKS: [usize; 3] = [1, 8, 32];

/// Frames per group, so groups roll as video would.
const GOP: u64 = 30;

/// A small payload keeps memory bounded across iterations; encryption cost is benched in moq-e2ee.
const PAYLOAD: &[u8] = &[0x5a; 64];

struct Fixture {
	_local: moq_net::broadcast::Producer,
	_output: moq_net::broadcast::Producer,
	_catalog: moq_mux::catalog::Producer,
	tracks: Vec<moq_mux::container::Producer<Container, hang::catalog::VideoConfig>>,
	producer: moq_mux::e2ee::Producer,
	waiter: kio::Waiter,
	frame: u64,
}

impl Fixture {
	fn new(tracks: usize) -> Self {
		let credential = moq_e2ee::Credential::new(moq_e2ee::credential::Config {
			context: Bytes::from_static(b"bench"),
			kid: 1,
			secret: [7; moq_e2ee::SECRET_LEN],
		})
		.unwrap();
		let mut local = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut local, Default::default()).unwrap();
		let tracks = (0..tracks)
			.map(|index| {
				let info = catalog.track_info(hang::catalog::PRIORITY.video);
				let track = local.create_track(format!("video{index}"), info).unwrap();
				let config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
				catalog.video(track, Container::Legacy(Kind::Video), config).unwrap()
			})
			.collect();
		let output = moq_net::broadcast::Info::new().produce();
		let producer = moq_mux::e2ee::Producer::new(local.consume(), credential.produce(), output.clone()).unwrap();

		let mut fixture = Self {
			_local: local,
			_output: output,
			_catalog: catalog,
			tracks,
			producer,
			waiter: kio::Waiter::noop(),
			frame: 0,
		};
		// Every track opens a group, so the catalog and every forwarder are live before measuring.
		for index in 0..fixture.tracks.len() {
			fixture.write(index);
		}
		fixture.frame += 1;
		fixture.carry();
		fixture
	}

	fn write(&mut self, track: usize) {
		self.tracks[track]
			.write(moq_mux::container::Frame {
				timestamp: moq_net::Timestamp::from_micros(self.frame * 33_333).unwrap(),
				duration: None,
				payload: Bytes::from_static(PAYLOAD),
				keyframe: self.frame.is_multiple_of(GOP),
			})
			.unwrap();
	}

	fn carry(&mut self) {
		assert!(self.producer.poll_run(&self.waiter).is_pending());
	}
}

fn carry(c: &mut Criterion) {
	let mut group = c.benchmark_group("e2ee_carry");
	group.throughput(Throughput::Elements(1));
	for tracks in TRACKS {
		let mut fixture = Fixture::new(tracks);
		group.bench_function(BenchmarkId::from_parameter(tracks), |b| {
			b.iter(|| {
				fixture.write(0);
				fixture.frame += 1;
				fixture.carry();
			})
		});
	}
	group.finish();
}

criterion_group!(benches, carry);
criterion_main!(benches);

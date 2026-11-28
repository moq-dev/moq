//! Path-heavy operations: building and slicing paths, decoding the messages that carry
//! them, resolving requests on an origin, and fanning announcements out to its cursors.
//!
//! Allocation counts are deterministic, so they show a regression as a higher number rather
//! than as noise. Paths span the inline limit: short (10 bytes), medium (22, the most that
//! fits inline), and long (47).
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench path`.

use std::fmt;

/// A path string and the name its case goes by.
#[derive(Clone, Copy)]
struct Sample {
	name: &'static str,
	path: &'static str,
}

impl fmt::Display for Sample {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(self.name)
	}
}

const SHORT: Sample = Sample {
	name: "short",
	path: "room/alice",
};
const MEDIUM: Sample = Sample {
	name: "medium",
	path: "anon/7f3a9c2e1b/camera",
};
const LONG: Sample = Sample {
	name: "long",
	path: "1234/private/channel_7/stream-health-1700000000",
};

mod path {
	use moq_net::{Path, path::Error};
	use swole::bench;

	use super::{LONG, MEDIUM, SHORT, Sample};

	const SAMPLES: [Sample; 3] = [SHORT, MEDIUM, LONG];

	fn string(sample: Sample) -> &'static str {
		sample.path
	}

	#[bench(args = SAMPLES, setup = string)]
	fn new(s: &'static str) -> Result<Path, Error> {
		Path::new(s)
	}

	fn path(sample: Sample) -> Path {
		Path::new(sample.path).unwrap()
	}

	#[bench(args = SAMPLES, setup = path)]
	fn clone(path: &Path) -> Path {
		path.clone()
	}

	fn prefixed(sample: Sample) -> (Path, Path) {
		let prefix = Path::new(sample.path.split('/').next().unwrap()).unwrap();
		(Path::new(sample.path).unwrap(), prefix)
	}

	#[bench(args = SAMPLES, setup = prefixed)]
	fn strip_prefix((path, prefix): &(Path, Path)) -> Path {
		path.strip_prefix(prefix).unwrap()
	}

	fn joined(sample: Sample) -> (Path, Path) {
		(Path::new(sample.path).unwrap(), Path::from_static("cam"))
	}

	#[bench(args = SAMPLES, setup = joined)]
	fn join((path, cam): &(Path, Path)) -> Result<Path, Error> {
		path.join(cam)
	}
}

mod decode {
	use std::fmt;

	use moq_net::{
		Path,
		fuzz::{Announced, Messages, decode_announces, decode_namespaces, encode_announces, encode_namespaces},
	};
	use swole::bench;

	fn lite() -> (Messages, Vec<u8>) {
		let messages = Messages::default();
		let mut lite = Vec::new();
		messages.encode_lite(&mut lite);
		(messages, lite)
	}

	#[bench(setup = lite)]
	fn lite_mix((messages, lite): &(Messages, Vec<u8>)) {
		messages.decode_lite(lite)
	}

	fn ietf() -> (Messages, Vec<u8>) {
		let messages = Messages::default();
		let mut ietf = Vec::new();
		messages.encode_ietf(&mut ietf);
		(messages, ietf)
	}

	#[bench(setup = ietf)]
	fn ietf_mix((messages, ietf): &(Messages, Vec<u8>)) {
		messages.decode_ietf(ietf)
	}

	/// Paths in an announce stream.
	const PATHS: u64 = 1000;

	/// How the stream's paths are shaped, named by their length class.
	#[derive(Clone, Copy)]
	struct Shape {
		name: &'static str,
		path: fn(u64) -> String,
	}

	impl fmt::Display for Shape {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			f.write_str(self.name)
		}
	}

	const SHAPES: [Shape; 3] = [
		Shape {
			name: "short",
			path: |i| format!("room/{i}"),
		},
		Shape {
			name: "medium",
			path: |i| format!("anon/{:010x}/camera", i * 0x9E37_79B9),
		},
		Shape {
			name: "long",
			path: |i| {
				format!(
					"{}/private/channel_{}/stream-health-{}",
					1000 + i % 8,
					i,
					1_700_000_000 + i
				)
			},
		},
	];

	fn paths(shape: Shape) -> Vec<Path> {
		(0..PATHS).map(|i| Path::try_from((shape.path)(i)).unwrap()).collect()
	}

	fn announces(shape: Shape, compress: bool) -> Vec<u8> {
		// Lite-07 names a claim's route in place of its hop chain.
		let stream: Vec<Announced> = paths(shape)
			.into_iter()
			.enumerate()
			.map(|(i, path)| {
				let hops = if compress {
					vec![]
				} else {
					vec![0x1111 + i as u64, 0x2222]
				};
				Announced::Start(path, hops)
			})
			.collect();
		encode_announces(&stream, compress)
	}

	fn lite06(shape: Shape) -> Vec<u8> {
		announces(shape, false)
	}

	fn lite07(shape: Shape) -> Vec<u8> {
		announces(shape, true)
	}

	#[bench(args = SHAPES, setup = lite06, items = |_: Shape| PATHS)]
	fn announce_lite06(data: &[u8]) -> Vec<Announced> {
		decode_announces(data, false)
	}

	#[bench(args = SHAPES, setup = lite07, items = |_: Shape| PATHS)]
	fn announce_lite07(data: &[u8]) -> Vec<Announced> {
		decode_announces(data, true)
	}

	fn namespaces(shape: Shape) -> Vec<u8> {
		encode_namespaces(&paths(shape))
	}

	#[bench(args = SHAPES, setup = namespaces, items = |_: Shape| PATHS)]
	fn announce_ietf(data: &[u8]) -> Vec<Path> {
		decode_namespaces(data)
	}
}

mod origin {
	use futures::FutureExt;
	use moq_net::{Error, Path, Prefixes, broadcast, origin};
	use swole::bench;

	use super::{LONG, SHORT, Sample};

	/// An origin serving 1000 routes under a covering one, and the paths each case requests.
	struct Requests {
		_producer: origin::Producer,
		driver: origin::Driver,
		consumer: origin::Consumer,
		_routes: Vec<origin::Route>,
		_covering: origin::Route,
		target: String,
		target_path: Path,
		rooted: origin::Consumer,
		rooted_target: String,
		missing: String,
	}

	impl Requests {
		fn new(sample: Sample) -> Self {
			let s = sample.path;
			let (producer, driver) = origin::Producer::new(origin::Config::default());
			let consumer = producer.consume();
			let routes = (0..1000)
				.map(|i| {
					producer
						.route(format!("{s}/{i}"), origin::RouteOptions::default())
						.unwrap()
				})
				.collect();
			let covering = producer.route(s, origin::RouteOptions::default()).unwrap();
			let target = format!("{s}/0");
			let target_path = Path::new(&target).unwrap();
			// A rooted handle joins its root onto every path it is given.
			let (root, rest) = s.split_once('/').unwrap();
			let rooted = consumer.scope(root, &Prefixes::all()).unwrap();
			Self {
				_producer: producer,
				driver,
				consumer,
				_routes: routes,
				_covering: covering,
				target,
				target_path,
				rooted,
				rooted_target: format!("{rest}/0"),
				missing: format!("{s}/missing"),
			}
		}

		/// Resolve a request on this origin's own broadcasts.
		fn local(&mut self, pending: kio::Pending<origin::Requesting>) -> broadcast::Consumer {
			self.driver
				.poll(moq_net::time::Instant::now(), &kio::Waiter::noop())
				.unwrap();
			pending.now_or_never().expect("resolves").expect("local broadcast")
		}

		fn request_str(&mut self) -> broadcast::Consumer {
			let pending = self.consumer.request(self.target.as_str(), None);
			self.local(pending)
		}

		fn request_path(&mut self) -> broadcast::Consumer {
			let pending = self.consumer.request(&self.target_path, None);
			self.local(pending)
		}

		fn request_rooted_str(&mut self) -> broadcast::Consumer {
			let pending = self.rooted.request(self.rooted_target.as_str(), None);
			self.local(pending)
		}

		fn unroutable_str(&mut self) {
			let result = self.consumer.request(self.missing.as_str(), None).now_or_never();
			assert!(matches!(result, Some(Err(Error::Unroutable))));
		}
	}

	const SAMPLES: [Sample; 2] = [SHORT, LONG];

	/// An origin warmed by a few of the requests a case makes, so lazily built state stays out.
	fn warmed(sample: Sample, request: fn(&mut Requests)) -> Requests {
		let mut requests = Requests::new(sample);
		for _ in 0..16 {
			request(&mut requests);
		}
		requests
	}

	fn warm_str(sample: Sample) -> Requests {
		warmed(sample, |requests| drop(requests.request_str()))
	}

	fn warm_path(sample: Sample) -> Requests {
		warmed(sample, |requests| drop(requests.request_path()))
	}

	fn warm_rooted_str(sample: Sample) -> Requests {
		warmed(sample, |requests| drop(requests.request_rooted_str()))
	}

	fn warm_unroutable_str(sample: Sample) -> Requests {
		warmed(sample, Requests::unroutable_str)
	}

	#[bench(args = SAMPLES, setup = warm_str)]
	fn request_str(requests: &mut Requests) -> broadcast::Consumer {
		requests.request_str()
	}

	#[bench(args = SAMPLES, setup = warm_path)]
	fn request_path(requests: &mut Requests) -> broadcast::Consumer {
		requests.request_path()
	}

	#[bench(args = SAMPLES, setup = warm_rooted_str)]
	fn request_rooted_str(requests: &mut Requests) -> broadcast::Consumer {
		requests.request_rooted_str()
	}

	#[bench(args = SAMPLES, setup = warm_unroutable_str)]
	fn unroutable_str(requests: &mut Requests) {
		requests.unroutable_str()
	}

	/// Cursors each announcement fans out to.
	const CURSORS: usize = 100;

	/// An origin with 100 routes under `room/` and cursors drained of them.
	struct Fanout {
		producer: origin::Producer,
		_driver: origin::Driver,
		_routes: Vec<origin::Route>,
		cursors: Vec<origin::Routes>,
		path: &'static str,
	}

	impl Fanout {
		/// Publish and retract a route, which every cursor reads.
		fn step(&mut self) {
			let publication = self.producer.route(self.path, origin::RouteOptions::default()).unwrap();
			for cursor in &mut self.cursors {
				cursor.next().now_or_never().flatten().expect("announce delivered");
			}
			drop(publication);
			for cursor in &mut self.cursors {
				cursor.next().now_or_never().flatten().expect("retract delivered");
			}
		}
	}

	fn fanout(sample: Sample) -> Fanout {
		let (producer, driver) = origin::Producer::new(origin::Config::default());
		let consumer = producer.consume();
		let routes = (0..100)
			.map(|i| {
				producer
					.route(format!("room/{i}"), origin::RouteOptions::default())
					.unwrap()
			})
			.collect();
		let mut cursors: Vec<origin::Routes> = (0..CURSORS).map(|_| consumer.routes()).collect();
		for cursor in &mut cursors {
			while cursor.next().now_or_never().flatten().is_some() {}
		}
		let mut fanout = Fanout {
			producer,
			_driver: driver,
			_routes: routes,
			cursors,
			path: sample.path,
		};
		for _ in 0..16 {
			fanout.step();
		}
		fanout
	}

	const FANOUT: [Sample; 2] = [
		Sample {
			name: "short",
			path: "room/incoming",
		},
		LONG,
	];

	/// One item per cursor, each reading a publish and a retract.
	#[bench(args = FANOUT, setup = fanout, items = |_: Sample| CURSORS)]
	fn announce_fanout(fanout: &mut Fanout) {
		fanout.step()
	}
}

fn main() {
	swole::main();
}

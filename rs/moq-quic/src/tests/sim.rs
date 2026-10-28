//! Random workloads over a seeded, impaired network, asserting what must hold on any path
//!
//! `MOQ_QUIC_SEED` and `MOQ_QUIC_CASES` pick the workloads, and `just rs quic-sim` sweeps a fresh
//! seed. A failure prints the shrunken case, whose `seed` replays the run exactly.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestCaseResult, TestRunner};

use super::*;

mod workloads;

/// One generated run
#[derive(Debug, Clone)]
struct Case {
	/// Replays every random choice the endpoints and links make
	seed: u64,
	latency: Duration,
	to_server: Link,
	to_client: Link,
	/// Both sides' receive limits
	limits: Limits,
	/// How long the receivers leave data unread between reads
	read_interval: Duration,
	streams: Vec<Spec>,
	extra: Option<workloads::Spec>,
}

#[derive(Debug, Clone, Copy)]
struct Limits {
	stream_window: u32,
	window: u32,
	max_streams: u32,
}

/// A unidirectional stream the workload sends
#[derive(Debug, Clone, Copy)]
struct Spec {
	from_client: bool,
	len: usize,
	priority: i64,
}

/// How long a run may take before it counts as stalled
const DEADLINE: Duration = Duration::from_secs(600);

#[test]
fn random_workloads() {
	check(case(link(true), limits()), |_| Ok(()));
}

/// Only a path that never loses or reorders keeps arrivals in the order the sender chose
#[test]
fn priority_on_a_clean_path() {
	let limits =
		(65_536u32..=262_144, 1_048_576u32..=4_194_304, 12u32..=16).prop_map(|(stream_window, window, max_streams)| {
			Limits {
				stream_window,
				window,
				max_streams,
			}
		});
	// Reads stamp arrival times, so a read every step catches an inversion within one interval
	let cases = case(link(false), limits).prop_map(|case| Case {
		read_interval: Duration::ZERO,
		extra: None,
		..case
	});
	check(cases, |outcome| {
		prop_assert_eq!(outcome.prioritized, [true; 2]);
		Ok(())
	});
}

/// Runs the cases `MOQ_QUIC_SEED` and `MOQ_QUIC_CASES` pick, then `property` on each outcome
fn check(cases: impl Strategy<Value = Case>, property: impl Fn(&Outcome) -> TestCaseResult) {
	let setting = |name: &str, fallback: u64| std::env::var(name).map_or(fallback, |value| value.parse().expect(name));
	let seed = setting("MOQ_QUIC_SEED", 0x6d6f71);
	let count = setting("MOQ_QUIC_CASES", 64) as u32;
	eprintln!("MOQ_QUIC_SEED={seed} MOQ_QUIC_CASES={count}");
	let mut runner = TestRunner::new(Config {
		cases: count,
		rng_seed: RngSeed::Fixed(seed),
		failure_persistence: None,
		..Default::default()
	});
	runner.run(&cases, |case| property(&run(&case)?)).unwrap();
}

/// Long enough for BBR3 to cycle through ProbeBW, whose timing is random too
#[test]
fn replays_from_seed() {
	let mut link = Link::default();
	link.bottleneck = Some(Bottleneck::new(500_000, 50_000, false));
	link.loss = 0.01;
	link.duplicate = 0.01;
	link.jitter = Duration::from_millis(2);
	link.reorder = 0.01;
	link.reorder_delay = Duration::from_millis(5);
	let case = |seed| Case {
		seed,
		latency: Duration::from_millis(20),
		to_server: link.clone(),
		to_client: link.clone(),
		limits: Limits {
			stream_window: 1 << 20,
			window: 4 << 20,
			max_streams: 4,
		},
		read_interval: Duration::ZERO,
		extra: None,
		streams: vec![
			Spec {
				from_client: true,
				len: 2 << 20,
				priority: 0,
			},
			Spec {
				from_client: false,
				len: 1 << 16,
				priority: 0,
			},
		],
	};

	let first = run(&case(1)).unwrap();
	assert_eq!(first, run(&case(1)).unwrap());
	assert_ne!(first, run(&case(2)).unwrap());
}

fn case(
	link: impl Strategy<Value = Link> + Clone,
	limits: impl Strategy<Value = Limits>,
) -> impl Strategy<Value = Case> {
	(
		any::<u64>(),
		0u64..=50_000,
		link.clone(),
		link,
		limits,
		0u64..=20_000,
		proptest::collection::vec(spec(), 1..=12),
		workloads::spec(),
	)
		.prop_map(
			|(seed, latency, to_server, to_client, limits, read_interval, streams, extra)| Case {
				seed,
				latency: Duration::from_micros(latency),
				to_server,
				to_client,
				limits,
				read_interval: Duration::from_micros(read_interval),
				streams,
				extra: Some(extra),
			},
		)
}

/// A direction of the path, which loses and reorders datagrams only when `lossy`
fn link(lossy: bool) -> impl Strategy<Value = Link> + Clone {
	let probability = |max: f64| prop_oneof![Just(0.0), 0.0..max];
	let unless_clean = move |max: f64| match lossy {
		true => probability(max).boxed(),
		false => Just(0.0).boxed(),
	};
	// A clean path's queue holds more than any workload sends, so it never drops
	let buffer = match lossy {
		true => (4_000u32..=200_000).boxed(),
		false => Just(16_000_000).boxed(),
	};
	(
		proptest::option::of((100_000u64..=10_000_000, buffer, any::<bool>())),
		unless_clean(0.1),
		probability(0.05),
		0u64..=10_000,
		unless_clean(0.1),
		0u64..=20_000,
	)
		.prop_map(|(bottleneck, loss, duplicate, jitter, reorder, reorder_delay)| {
			let mut link = Link::default();
			link.bottleneck = bottleneck.map(|(rate, buffer, ce)| Bottleneck::new(rate, buffer, ce));
			link.loss = loss;
			link.duplicate = duplicate;
			link.jitter = Duration::from_micros(jitter);
			link.reorder = reorder;
			link.reorder_delay = Duration::from_micros(reorder_delay);
			link
		})
}

fn limits() -> impl Strategy<Value = Limits> {
	(1_200u32..=262_144, 4_800u32..=1_048_576, 1u32..=8).prop_map(|(stream_window, window, max_streams)| Limits {
		stream_window,
		window,
		max_streams,
	})
}

fn spec() -> impl Strategy<Value = Spec> {
	(any::<bool>(), 0usize..=65_536, -2i64..=2).prop_map(|(from_client, len, priority)| Spec {
		from_client,
		len,
		priority,
	})
}

/// What a run looked like, which a replay of its case must match exactly
#[derive(Debug, PartialEq)]
struct Outcome {
	/// Whether each side, client then server, sent in a way priority alone orders
	prioritized: [bool; 2],
	elapsed: Duration,
	client: String,
	server: String,
}

/// One side of the connection, sending its streams and receiving the peer's
struct Peer {
	client: bool,
	ch: ConnectionHandle,
	/// The streams this side sends, in the order it opens them
	sends: Vec<Sending>,
	/// The streams this side has accepted, in the order the peer opened them
	recvs: Vec<Receiving>,
}

struct Sending {
	spec: Spec,
	id: Option<StreamId>,
	written: usize,
	finished: bool,
}

#[derive(Default)]
struct Receiving {
	read: usize,
	/// When the first byte was read
	first: Option<Instant>,
	/// When the stream was read to its end
	done: Option<Instant>,
}

impl Peer {
	fn conn<'a>(&self, pair: &'a mut Pair) -> &'a mut Connection {
		match self.client {
			true => pair.client_conn_mut(self.ch),
			false => pair.server_conn_mut(self.ch),
		}
	}

	/// Opens streams in order as the peer's limit allows, then writes and finishes what fits
	fn send(&mut self, pair: &mut Pair) {
		let conn = self.conn(pair);
		for (index, sending) in self.sends.iter_mut().enumerate() {
			let id = match sending.id {
				Some(id) => id,
				None => match conn.streams().open(Dir::Uni, Default::default()) {
					Some(id) => {
						assert_eq!(id.index(), index as u64);
						conn.send_stream(id).set_priority(sending.spec.priority).unwrap();
						sending.id = Some(id);
						id
					}
					None => return,
				},
			};
			let mut stream = conn.send_stream(id);
			while sending.written < sending.spec.len {
				let end = sending.spec.len.min(sending.written + 65_536);
				let data = payload(index, sending.written..end);
				match stream.write(&data) {
					Ok(written) => sending.written += written,
					Err(WriteError::Blocked) => break,
					Err(error) => panic!("writing stream {index}: {error}"),
				}
			}
			if sending.written == sending.spec.len && !sending.finished {
				stream.finish().unwrap();
				sending.finished = true;
			}
		}
	}

	/// Accepts every stream the peer opened and reads all that has arrived, checking each byte
	/// arrives once and in order, within the flow control and stream limits
	fn receive(&mut self, pair: &mut Pair, peer: &[Sending], limits: Limits) -> Result<(), TestCaseError> {
		let now = pair.time;
		let conn = self.conn(pair);
		while let Some(id) = conn.streams().accept(Dir::Uni) {
			prop_assert_eq!(id.index(), self.recvs.len() as u64);
			self.recvs.push(Receiving::default());
		}

		let open = self.recvs.iter().filter(|r| r.done.is_none()).count();
		prop_assert!(
			open as u32 <= limits.max_streams,
			"{open} streams open past the limit of {}",
			limits.max_streams
		);

		let mut read = 0;
		for (index, receiving) in self.recvs.iter_mut().enumerate() {
			if receiving.done.is_some() {
				continue;
			}
			let id = StreamId::new(
				if self.client { Side::Server } else { Side::Client },
				Dir::Uni,
				index as u64,
			);
			let len = peer[index].spec.len;
			let before = receiving.read;
			let mut stream = conn.recv_stream(id);
			let mut chunks = stream.read().unwrap();
			loop {
				match chunks.next(usize::MAX) {
					Ok(Some(chunk)) => {
						let end = receiving.read + chunk.bytes.len();
						prop_assert!(end <= len, "stream {index} ran past its {len} bytes");
						prop_assert!(
							chunk.bytes == payload(index, receiving.read..end),
							"stream {index} corrupted at {}",
							receiving.read
						);
						receiving.read = end;
						receiving.first.get_or_insert(now);
					}
					Ok(None) => {
						prop_assert_eq!(receiving.read, len, "stream {} ended early", index);
						receiving.first.get_or_insert(now);
						receiving.done = Some(now);
						break;
					}
					Err(ReadError::Blocked) => break,
					Err(error) => panic!("reading stream {index}: {error}"),
				}
			}
			let _ = chunks.finalize();

			let burst = receiving.read - before;
			prop_assert!(
				burst <= limits.stream_window as usize,
				"stream {index} buffered {burst} bytes past its window"
			);
			read += burst;
		}
		prop_assert!(
			read <= limits.window as usize,
			"buffered {read} bytes past the connection window"
		);
		Ok(())
	}

	/// Drains events, failing if the connection was lost
	fn poll(&self, pair: &mut Pair) -> Result<(), TestCaseError> {
		while let Some(event) = self.conn(pair).poll() {
			if let Event::ConnectionLost { reason } = event {
				return Err(TestCaseError::fail(format!(
					"{} lost the connection: {reason}",
					self.side()
				)));
			}
		}
		Ok(())
	}

	fn done(&self, peer: &[Sending]) -> bool {
		self.recvs.len() == peer.len() && self.recvs.iter().all(|r| r.done.is_some())
	}

	fn side(&self) -> &'static str {
		if self.client { "client" } else { "server" }
	}
}

/// The bytes at `range` of stream `index`, distinct per stream and offset
fn payload(index: usize, range: ops::Range<usize>) -> Vec<u8> {
	range.map(|i| (i % 251) as u8 ^ (index as u8)).collect()
}

fn run(case: &Case) -> Result<Outcome, TestCaseError> {
	entropy::reseed(case.seed);
	let mut transport = transport::Config::default();
	transport.limits.stream_receive_window = NonZeroU64::new(case.limits.stream_window.into());
	transport.limits.receive_window = NonZeroU64::new(case.limits.window.into());
	transport.limits.max_streams = NonZeroU64::new(case.limits.max_streams.into());
	transport.datagram.receive_buffer = Some(65_536);
	transport.datagram.send_buffer = 65_536;
	transport.reset_stream_at = true;

	let mut server_config = server_config();
	server_config.transport = transport.clone();
	let mut pair = Pair::new(Default::default(), server_config);
	pair.latency = case.latency;
	pair.to_server = case.to_server.clone();
	pair.to_client = case.to_client.clone();
	let start = pair.time;

	let mut client_config = client_config();
	client_config.transport = transport;
	let (client_ch, server_ch) = pair.connect_with(client_config);

	let sends = |from_client: bool| {
		case.streams
			.iter()
			.filter(|spec| spec.from_client == from_client)
			.map(|&spec| Sending {
				spec,
				id: None,
				written: 0,
				finished: false,
			})
			.collect()
	};
	let mut client = Peer {
		client: true,
		ch: client_ch,
		sends: sends(true),
		recvs: Vec::new(),
	};
	let mut server = Peer {
		client: false,
		ch: server_ch,
		sends: sends(false),
		recvs: Vec::new(),
	};

	let mut extra = case
		.extra
		.as_ref()
		.map(|spec| workloads::Run::new(spec, client_ch, server_ch));

	client.send(&mut pair);
	server.send(&mut pair);
	// Priority alone orders the data only if it was all waiting to be sent at once
	let written = [&client, &server].map(|peer| case.extra.is_none() && peer.sends.iter().all(|s| s.finished));

	let mut next_read = pair.time;
	loop {
		client.send(&mut pair);
		server.send(&mut pair);
		if let Some(extra) = &mut extra {
			extra.send(&mut pair)?;
		}
		if pair.time >= next_read {
			client.receive(&mut pair, &server.sends, case.limits)?;
			server.receive(&mut pair, &client.sends, case.limits)?;
			if let Some(extra) = &mut extra {
				extra.receive(&mut pair)?;
			}
			next_read = pair.time + case.read_interval;
		}
		client.poll(&mut pair)?;
		server.poll(&mut pair)?;
		if client.done(&server.sends) && server.done(&client.sends) && extra.as_ref().is_none_or(|extra| extra.done()) {
			break;
		}

		prop_assert!(pair.time - start < DEADLINE, "stalled for {DEADLINE:?}");
		if !pair.step() {
			// Both connections are idle: nothing happens until a read or an arrival
			let next = [
				Some(next_read).filter(|&t| t > pair.time),
				pair.client.inbound.front().map(|x| x.0),
				pair.server.inbound.front().map(|x| x.0),
			]
			.into_iter()
			.flatten()
			.min();
			let Some(next) = next else {
				return Err(TestCaseError::fail("deadlocked with streams unread"));
			};
			pair.time = next;
		}
	}

	if let Some(extra) = &mut extra {
		extra.finish(&mut pair)?;
	}

	// ...and the path delivered every datagram in order
	let clean = |link: &Link| {
		link.loss == 0.0 && link.reorder == 0.0 && link.bottleneck.as_ref().is_none_or(|b| b.stats.dropped == 0)
	};
	let clean = clean(&pair.to_server) && clean(&pair.to_client);
	let prioritized = written.map(|written| written && clean);
	for (sender, receiver, prioritized) in [(&client, &server, prioritized[0]), (&server, &client, prioritized[1])] {
		if prioritized {
			check_priority(&sender.sends, &receiver.recvs)?;
		}
	}

	// Heal the path so the close cannot be lost, including to a full bottleneck's tail drop
	for link in [&mut pair.to_server, &mut pair.to_client] {
		link.bottleneck = None;
		link.loss = 0.0;
		link.reorder = 0.0;
	}
	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(0), String::new());
	pair.drive();
	let reason = loop {
		match pair.server_conn_mut(server_ch).poll() {
			Some(Event::ConnectionLost { reason }) => break reason,
			Some(_) => {}
			None => return Err(TestCaseError::fail("server never saw the close")),
		}
	};
	prop_assert!(
		matches!(&reason, ConnectionError::ApplicationClosed(close) if close.error_code == VarInt(0)),
		"server saw {reason}"
	);
	prop_assert!(pair.client_conn_mut(client_ch).is_drained());
	prop_assert!(pair.server_conn_mut(server_ch).is_drained());

	Ok(Outcome {
		prioritized,
		elapsed: pair.time - start,
		client: format!("{:?}", pair.client_conn_mut(client_ch).stats()),
		server: format!("{:?}", pair.server_conn_mut(server_ch).stats()),
	})
}

/// A higher-priority stream finishes arriving before any byte of a lower-priority one
fn check_priority(sends: &[Sending], recvs: &[Receiving]) -> Result<(), TestCaseError> {
	for (high, high_recv) in sends.iter().zip(recvs) {
		for (low, low_recv) in sends.iter().zip(recvs) {
			if high.spec.priority <= low.spec.priority || low.spec.len == 0 {
				continue;
			}
			prop_assert!(
				high_recv.done <= low_recv.first,
				"priority {} finished at {:?}, after priority {} started at {:?}",
				high.spec.priority,
				high_recv.done,
				low.spec.priority,
				low_recv.first
			);
		}
	}
	Ok(())
}

//! Random workloads over a seeded, impaired network, asserting what must hold on any path
//!
//! `MOQ_QUIC_SEED` and `MOQ_QUIC_CASES` pick the workloads, and `just rs quic-sim` sweeps a fresh
//! seed. A failure prints the shrunken case, whose `seed` replays the run exactly.

use std::collections::VecDeque;

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

/// A handshake under up to the QUIC Interop Runner's 30% loss either completes or ends only
/// after a side heard nothing for the whole handshake timeout, and the server leaves the client's
/// initial CID for its next in order, retiring nothing else
#[test]
fn handshake_under_loss() {
	let link = (0.0..=0.3, probability(0.05), probability(0.1), 0u64..=20_000).prop_map(
		|(loss, duplicate, reorder, reorder_delay)| {
			let mut link = Link::default();
			link.loss = loss;
			link.duplicate = duplicate;
			link.reorder = reorder;
			link.reorder_delay = Duration::from_micros(reorder_delay);
			link
		},
	);
	let cases = (any::<u64>(), 0u64..=50_000, link.clone(), link);
	sweep(cases, |(seed, latency, to_server, to_client)| {
		handshake(seed, Duration::from_micros(latency), to_server, to_client)
	});
}

fn handshake(seed: u64, latency: Duration, to_server: Link, to_client: Link) -> TestCaseResult {
	entropy::reseed(seed);
	let bound = transport::Config::default().handshake_timeout;
	let mut pair = Pair::new(Default::default(), server_config());
	pair.latency = latency;
	pair.to_server = to_server;
	pair.to_client = to_client;
	let start = pair.time;
	let client_ch = pair.begin_connect(client_config());

	// When each side last processed a packet from the other, to tell a give-up from a dead
	// network: an arrival it cannot decrypt yet is no sign of life
	let mut heard = [start; 2];
	let mut frames = [String::new(), String::new()];
	let server_ch = loop {
		let now = pair.time;
		let stepped = pair.step();
		let server_ch = pair.server.connections.keys().next().copied();
		for (index, ch) in [Some(client_ch), server_ch].into_iter().enumerate() {
			let Some(ch) = ch else { continue };
			let conn = match index {
				0 => pair.client_conn_mut(ch),
				_ => pair.server_conn_mut(ch),
			};
			let received = format!("{:?}", conn.stats().frame_rx);
			if received != frames[index] {
				frames[index] = received;
				heard[index] = now;
			}
			let Some(error) = conn.error().cloned() else { continue };
			prop_assert!(
				matches!(error, ConnectionError::HandshakeTimeout),
				"side {index} failed: {error}"
			);
			// The silence right before it gave up, not any earlier one
			let silence = now - heard[index];
			prop_assert!(silence >= bound, "side {index} gave up after {silence:?} of silence");
			return Ok(());
		}
		if let Some(server_ch) = server_ch
			&& is_confirmed(pair.client_conn_mut(client_ch))
			&& is_connected(pair.server_conn_mut(server_ch))
		{
			break server_ch;
		}
		prop_assert!(pair.time - start < DEADLINE, "stalled for {DEADLINE:?}");
		if !stepped {
			// Only an idle timer or an arrival is left
			let next = [
				pair.client.inbound.front().map(|x| x.0),
				pair.server.inbound.front().map(|x| x.0),
				pair.client.next_wakeup(),
				pair.server.next_wakeup(),
			]
			.into_iter()
			.flatten()
			.min();
			let Some(next) = next else {
				return Err(TestCaseError::fail("deadlocked mid-handshake"));
			};
			pair.time = next;
		}
	};

	// Heal the path so every NEW_CONNECTION_ID and RETIRE_CONNECTION_ID lands
	for link in [&mut pair.to_server, &mut pair.to_client] {
		link.loss = 0.0;
		link.reorder = 0.0;
	}
	pair.drive();
	prop_assert_eq!(pair.server_conn_mut(server_ch).active_rem_cid_seq(), 1);
	prop_assert_eq!(pair.client_conn_mut(client_ch).active_local_cid_seq().0, 1);
	Ok(())
}

/// A corrupted ServerHello costs one round trip, not a PTO: the Handshake packets coalesced with
/// it reach the client, which cannot read them yet and probes in Initial at once, and that probe
/// makes the server re-send the ServerHello at once (RFC 9002 §6.2.3), with the Handshake flight
/// the client discarded coalesced behind it.
#[test]
fn a_corrupted_server_hello_is_resent_on_the_clients_probe() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	pair.latency = Duration::from_millis(50);
	let client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	arrive(&mut pair.server, &mut pair.time);
	pair.server.drive(pair.time, pair.client.addr);
	assert_eq!(corrupt(&mut pair.server.outbound, SpaceId::Initial), 1);
	pair.drive_server();

	// Neither side's loss detection timer fires below, so every reply is to an arrival
	let sent = pair.time;
	arrive(&mut pair.client, &mut pair.time);
	let probes = pair.client_conn_mut(client_ch).stats().frame_tx;
	pair.client.drive(pair.time, pair.server.addr);
	let after = pair.client_conn_mut(client_ch).stats().frame_tx;
	assert!(
		after.ping + after.crypto > probes.ping + probes.crypto,
		"the client did not probe in Initial"
	);
	assert!(spaces(&pair.client.outbound).contains(&SpaceId::Initial));
	pair.drive_client();

	arrive(&mut pair.server, &mut pair.time);
	let server_ch = *pair.server.connections.keys().next().unwrap();
	let resent = pair.server_conn_mut(server_ch).stats().frame_tx.crypto;
	pair.server.drive(pair.time, pair.client.addr);
	assert!(
		pair.server_conn_mut(server_ch).stats().frame_tx.crypto > resent,
		"the server answered the probe without its ServerHello"
	);
	let first: Vec<_> = packets(&pair.server.outbound[0].1)
		.into_iter()
		.map(|(space, _)| space)
		.collect();
	assert_eq!(
		first[..2],
		[SpaceId::Initial, SpaceId::Handshake],
		"the ServerHello and the Handshake flight share one datagram"
	);
	pair.drive_server();

	arrive(&mut pair.client, &mut pair.time);
	pair.client.drive(pair.time, pair.server.addr);
	assert!(
		is_connected(pair.client_conn_mut(client_ch)),
		"the client still lacks part of the server's flight"
	);
	assert_eq!(pair.time - sent, 3 * pair.latency);

	pair.drive();
	assert!(is_confirmed(pair.client_conn_mut(client_ch)));
	assert!(is_connected(pair.server_conn_mut(server_ch)));
}

/// A client's 1-RTT packet reaching the server ahead of its Finished means the Finished is late
/// or lost. The server answers with a Handshake PING acknowledging what the client sent there,
/// and the client, seeing its Finished still unacknowledged, re-sends it at once rather than on a
/// PTO that Initial losses may have backed off past the idle timeout.
#[test]
fn a_corrupted_finished_is_resent_when_1rtt_overtakes_it() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	pair.latency = Duration::from_millis(50);
	let client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	arrive(&mut pair.server, &mut pair.time);
	pair.drive_server();
	arrive(&mut pair.client, &mut pair.time);
	pair.client.drive(pair.time, pair.server.addr);
	assert_eq!(corrupt(&mut pair.client.outbound, SpaceId::Handshake), 1);
	assert!(
		spaces(&pair.client.outbound).contains(&SpaceId::Data),
		"the client sent no 1-RTT packet with its Finished"
	);
	pair.drive_client();

	let sent = pair.time;
	arrive(&mut pair.server, &mut pair.time);
	let server_ch = *pair.server.connections.keys().next().unwrap();
	let pings = pair.server_conn_mut(server_ch).stats().frame_tx.ping;
	pair.server.drive(pair.time, pair.client.addr);
	assert!(!is_connected(pair.server_conn_mut(server_ch)));
	assert!(
		pair.server_conn_mut(server_ch).stats().frame_tx.ping > pings,
		"the server waited for the Finished in silence"
	);
	assert!(spaces(&pair.server.outbound).contains(&SpaceId::Handshake));
	pair.drive_server();

	arrive(&mut pair.client, &mut pair.time);
	let finished = pair.client_conn_mut(client_ch).stats().frame_tx.crypto;
	pair.client.drive(pair.time, pair.server.addr);
	assert!(
		pair.client_conn_mut(client_ch).stats().frame_tx.crypto > finished,
		"the client did not re-send its Finished"
	);
	assert_eq!(pair.time - sent, 2 * pair.latency);

	pair.drive();
	assert!(is_connected(pair.server_conn_mut(server_ch)));
	assert!(is_confirmed(pair.client_conn_mut(client_ch)));
}

/// Moves `time` to the next datagram `endpoint` receives
fn arrive(endpoint: &mut TestEndpoint, time: &mut Instant) {
	*time = endpoint.inbound.front().expect("a datagram on its way").0;
}

/// Each coalesced packet's space, for every datagram in `outbound`
fn spaces(outbound: &VecDeque<(Transmit, Bytes)>) -> Vec<SpaceId> {
	outbound
		.iter()
		.flat_map(|(_, datagram)| packets(datagram))
		.map(|(space, _)| space)
		.collect()
}

/// Flips a bit in the AEAD tag of every packet in `space`, which the receiver then fails to
/// authenticate and drops, still reading the packets coalesced with it. Returns how many.
fn corrupt(outbound: &mut VecDeque<(Transmit, Bytes)>, space: SpaceId) -> usize {
	let mut count = 0;
	for (_, datagram) in outbound.iter_mut() {
		let mut bytes = datagram.to_vec();
		for (_, range) in packets(datagram).into_iter().filter(|(s, _)| *s == space) {
			bytes[range.end - 1] ^= 1;
			count += 1;
		}
		*datagram = bytes.into();
	}
	count
}

/// Each coalesced packet of `datagram`: its space and where it lies
fn packets(datagram: &[u8]) -> Vec<(SpaceId, ops::Range<usize>)> {
	use crate::packet::{FixedLengthConnectionIdParser, PartialDecode};
	let parser = FixedLengthConnectionIdParser::new(RandomConnectionIdGenerator::default().cid_len());
	let mut packets = Vec::new();
	let mut rest = Some(BytesMut::from(datagram));
	let mut start = 0;
	while let Some(bytes) = rest.take() {
		let (decoded, tail) = PartialDecode::new(bytes, &parser, true).expect("a valid datagram");
		let len = decoded.data().len();
		packets.push((decoded.space().expect("a protected packet"), start..start + len));
		start += len;
		rest = tail;
	}
	packets
}

fn probability(max: f64) -> impl Strategy<Value = f64> + Clone {
	prop_oneof![Just(0.0), 0.0..max]
}

/// Runs the cases `MOQ_QUIC_SEED` and `MOQ_QUIC_CASES` pick, then `property` on each outcome
fn check(cases: impl Strategy<Value = Case>, property: impl Fn(&Outcome) -> TestCaseResult) {
	sweep(cases, |case| property(&run(&case)?));
}

/// Runs `test` on the cases `MOQ_QUIC_SEED` and `MOQ_QUIC_CASES` pick
fn sweep<S: Strategy>(cases: S, test: impl Fn(S::Value) -> TestCaseResult) {
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
	runner.run(&cases, test).unwrap();
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
				let end = sending.spec.len.min(sending.written + PAYLOAD_MAX);
				match stream.write(&mut payload(index, sending.written..end)) {
					Ok(written) => sending.written += written,
					Err(send::Error::Blocked) => break,
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
							is_payload(index, receiving.read, &chunk.bytes),
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
					Err(recv::Error::Blocked) => break,
					Err(error) => panic!("reading stream {index}: {error}"),
				}
			}
			chunks.finalize();

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

	/// Fails if the connection was lost
	fn check_alive(&self, pair: &mut Pair) -> Result<(), TestCaseError> {
		if let Some(reason) = self.conn(pair).error() {
			return Err(TestCaseError::fail(format!(
				"{} lost the connection: {reason}",
				self.side()
			)));
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

/// The longest range `payload` serves
const PAYLOAD_MAX: usize = 65_536;

/// The bytes at `range` of stream `index`, distinct per stream and offset
///
/// Each stream reads one cycle of prime length from its own rotation, so streams fewer than 251
/// apart differ at every offset. Slicing a shared buffer keeps the per-step writes and checks from
/// dominating the simulation.
fn payload(index: usize, range: ops::Range<usize>) -> &'static [u8] {
	static CYCLE: std::sync::LazyLock<Vec<u8>> =
		std::sync::LazyLock::new(|| (0..251 + PAYLOAD_MAX).map(|i| (i % 251) as u8).collect());
	let start = (index + range.start) % 251;
	&CYCLE[start..][..range.len()]
}

/// Whether `bytes` are stream `index`'s payload from `offset`, however long the chunk
fn is_payload(index: usize, offset: usize, bytes: &[u8]) -> bool {
	bytes.chunks(PAYLOAD_MAX).enumerate().all(|(i, piece)| {
		let start = offset + i * PAYLOAD_MAX;
		piece == payload(index, start..start + piece.len())
	})
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

	let mut client_config = client_config();
	client_config.transport = transport;
	let (client_ch, server_ch) = pair.connect_with(client_config);

	// Workloads start connected; `handshake_under_loss` owns flight loss.
	pair.to_server = case.to_server.clone();
	pair.to_client = case.to_client.clone();
	let start = pair.time;

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
			next_read = pair.time.saturating_add(case.read_interval);
		}
		client.check_alive(&mut pair)?;
		server.check_alive(&mut pair)?;
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
	let Some(reason) = pair.server_conn_mut(server_ch).error().cloned() else {
		return Err(TestCaseError::fail("server never saw the close"));
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

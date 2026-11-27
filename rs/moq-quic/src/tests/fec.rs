//! Experimental forward error correction ([`transport::fec`]): live video on streams over a
//! lossy path, with repair packets and without.
//!
//! Every run sends the same media, one stream per group of frames, over the same seeded path,
//! and measures how long after it was written each frame is read, in order, at the receiver.
//! A frame that takes a frame interval longer than the path's delay counts as late: the next
//! frame was due before it arrived. The gate tests run a few seconds on one seed; `compare` is
//! the `#[ignore]`d sweep that prints the table the experiment's write-up quotes.

use std::collections::HashMap;

use super::*;
use crate::{
	congestion::{Unlimited, gcc},
	sim::Fades,
	transport::fec::{self, Code},
};

/// The media's bitrate, 4 Mbit/s
const MEDIA: u64 = 500_000;
const FPS: u64 = 30;
/// Frames per group, each group on its own stream
const GOP: u64 = 60;
const FRAME: u64 = MEDIA / FPS;

#[derive(Clone)]
struct Scenario {
	seed: u64,
	/// One-way delay
	latency: Duration,
	/// The probability each packet is lost on its own, in both directions
	loss: f64,
	/// Fades, in both directions
	fades: Option<Fades>,
	/// What the sender sends, or `None` to send no repairs; the receiver always offers to
	/// rebuild
	send: Option<fec::Send>,
	controller: Controller,
	duration: Duration,
}

impl Default for Scenario {
	fn default() -> Self {
		Self {
			seed: 1,
			latency: Duration::from_millis(30),
			loss: 0.0,
			fades: None,
			send: None,
			controller: Controller::Unlimited,
			duration: Duration::from_secs(5),
		}
	}
}

/// The sender's congestion controller
#[derive(Clone, Copy, Debug)]
enum Controller {
	/// None, and no pacing either: every frame leaves at once, which leaves loss recovery
	/// alone to compare
	Unlimited,
	/// The default, which reads loss as congestion
	Bbr3,
	/// Delay-based, reading the receive timestamps it asks for
	Gcc,
}

#[derive(Debug, Default)]
struct Outcome {
	frames: u64,
	/// Each frame's delay from written to read, sorted
	delays: Vec<Duration>,
	/// Frames read later than the path's delay and a frame interval, or never
	late: u64,
	/// Packets the sender sent and declared lost
	sent: u64,
	lost: u64,
	/// REPAIR frames sent, and packets the receiver rebuilt from them
	repairs: u64,
	rebuilt: u64,
	/// UDP payload bytes the sender sent
	wire: u64,
}

impl Outcome {
	fn percentile(&self, p: f64) -> Duration {
		let index = ((self.delays.len() as f64 * p) as usize).min(self.delays.len().saturating_sub(1));
		self.delays.get(index).copied().unwrap_or_default()
	}

	fn late(&self) -> f64 {
		self.late as f64 / self.frames as f64
	}
}

/// A group the sender has opened
struct Group {
	id: StreamId,
	/// When each of its frames was written
	written: Vec<Instant>,
	/// Frame bytes the stream had no credit for yet
	backlog: Vec<u8>,
	ended: bool,
	finished: bool,
}

/// A group the receiver is reading
#[derive(Default)]
struct Reading {
	read: u64,
	done: bool,
}

fn run(scenario: &Scenario) -> Outcome {
	let _guard = subscribe();
	entropy::reseed(scenario.seed);
	let congestion = |config: &mut transport::Config| match scenario.controller {
		Controller::Unlimited => config.congestion = Arc::new(Unlimited),
		Controller::Bbr3 => {}
		Controller::Gcc => {
			config.congestion = Arc::new(gcc::Config::default());
			config.receive_timestamps = NonZeroU32::new(32);
		}
	};

	let mut server = server_config();
	server.transport.limits.max_streams = NonZeroU64::new(10_000);
	server.transport.fec = Some(fec::Config {
		send: None,
		..fec::Config::default()
	});
	congestion(&mut server.transport);
	let mut pair = Pair::new(Default::default(), server);
	pair.latency = scenario.latency;

	let mut client = client_config();
	client.transport.fec = Some(fec::Config {
		send: scenario.send.clone(),
		..fec::Config::default()
	});
	congestion(&mut client.transport);
	let (client_ch, server_ch) = pair.connect_with(client);
	for link in [&mut pair.to_server, &mut pair.to_client] {
		link.loss = scenario.loss;
		link.fades = scenario.fades;
	}

	let start = pair.time;
	let end = start.saturating_add(scenario.duration);
	// Long enough after the last frame for every retransmission to land
	let stop = end.saturating_add(Duration::from_secs(5));
	let late = scenario.latency.saturating_add(Duration::from_secs(1) / FPS as u32);
	let frame_at = |n: u64| start.saturating_add(Duration::from_nanos(n * 1_000_000_000 / FPS));
	let mut outcome = Outcome::default();
	let mut groups: Vec<Group> = Vec::new();
	let mut readings: HashMap<StreamId, Reading> = HashMap::new();
	let mut next_frame = 0u64;

	loop {
		let now = pair.time;
		while frame_at(next_frame) <= now && frame_at(next_frame) < end {
			if next_frame.is_multiple_of(GOP) {
				if let Some(group) = groups.last_mut() {
					group.ended = true;
				}
				let options = send::Options {
					group: None,
					priority: groups.len() as i64,
				};
				let id = pair
					.client_streams(client_ch)
					.open(Dir::Uni, options)
					.expect("stream limit");
				groups.push(Group {
					id,
					written: Vec::new(),
					backlog: Vec::new(),
					ended: false,
					finished: false,
				});
			}
			let group = groups.last_mut().unwrap();
			group.backlog.extend([(next_frame % 251) as u8].repeat(FRAME as usize));
			group.written.push(now);
			outcome.frames += 1;
			next_frame += 1;
		}
		if now >= end
			&& let Some(group) = groups.last_mut()
		{
			group.ended = true;
		}
		for group in groups.iter_mut().filter(|group| !group.finished) {
			let mut stream = pair.client_send(client_ch, group.id);
			while !group.backlog.is_empty() {
				match stream.try_write(&mut &group.backlog[..]) {
					Poll::Ready(Ok(n)) => drop(group.backlog.drain(..n)),
					Poll::Pending => break,
					Poll::Ready(Err(e)) => panic!("write: {e}"),
				}
			}
			if group.ended && group.backlog.is_empty() {
				stream.finish().unwrap();
				group.finished = true;
			}
		}

		pair.drive_client();
		pair.drive_server();

		let conn = pair.server_conn_mut(server_ch);
		while let Some(id) = conn.streams().accept(Dir::Uni) {
			readings.insert(id, Reading::default());
		}
		for (&id, reading) in readings.iter_mut().filter(|(_, reading)| !reading.done) {
			// The sender opens each group's stream in order
			let index = id.index() - groups[0].id.index();
			let mut stream = conn.recv_stream(id);
			let mut chunks = stream.read().unwrap();
			loop {
				match chunks.try_next(usize::MAX) {
					Poll::Ready(Ok(Some(chunk))) => {
						// Every byte is the one written, whether it arrived or was rebuilt
						for (i, &byte) in chunk.bytes.iter().enumerate() {
							let frame = index * GOP + (reading.read + i as u64) / FRAME;
							assert_eq!(byte, (frame % 251) as u8, "stream {id} at {}", reading.read + i as u64);
						}
						let before = reading.read / FRAME;
						reading.read += chunk.bytes.len() as u64;
						for frame in before..reading.read / FRAME {
							let delay = now - groups[index as usize].written[frame as usize];
							outcome.late += (delay > late) as u64;
							outcome.delays.push(delay);
						}
					}
					Poll::Ready(Ok(None)) => {
						reading.done = true;
						break;
					}
					Poll::Pending => break,
					Poll::Ready(Err(e)) => panic!("read: {e}"),
				}
			}
			chunks.finalize();
		}

		let delivered = outcome.delays.len() as u64 == outcome.frames;
		if now >= stop || (now >= end && delivered) {
			break;
		}
		let wakeups = [
			pair.client.next_wakeup(),
			pair.server.next_wakeup(),
			(frame_at(next_frame) < end).then(|| frame_at(next_frame)),
			Some(if now < end { end } else { stop }),
		];
		pair.time = pair.time.max(wakeups.into_iter().flatten().min().unwrap());
	}

	// A frame that never arrived is late
	outcome.late += outcome.frames - outcome.delays.len() as u64;
	outcome.delays.sort();
	let stats = pair.client_conn_mut(client_ch).stats();
	outcome.sent = stats.path.sent_packets;
	outcome.lost = stats.path.lost_packets;
	outcome.repairs = stats.frame_tx.repair;
	outcome.wire = stats.udp_tx.bytes;
	let stats = pair.server_conn_mut(server_ch).stats();
	outcome.rebuilt = stats.path.rebuilt_packets;
	outcome
}

fn reed_solomon(overhead: u8) -> Option<fec::Send> {
	Some(fec::Send {
		code: Code::ReedSolomon,
		overhead,
		..fec::Send::default()
	})
}

fn xor(block: u8) -> Option<fec::Send> {
	Some(fec::Send {
		code: Code::Xor,
		block,
		..fec::Send::default()
	})
}

/// Repairs rebuild lost packets, every byte arrives as written, and fewer frames are late than
/// without them
#[test]
fn rebuilds_lost_packets() {
	for send in [xor(8), reed_solomon(20)] {
		let scenario = Scenario {
			loss: 0.02,
			send,
			..Scenario::default()
		};
		let with = run(&scenario);
		let without = run(&Scenario { send: None, ..scenario });
		info!(?with, "with repairs");
		info!(?without, "without repairs");
		assert_eq!(with.delays.len() as u64, with.frames);
		assert_eq!(without.delays.len() as u64, without.frames);
		assert_eq!(without.repairs, 0);
		assert_eq!(without.rebuilt, 0);
		assert!(with.rebuilt > 0, "nothing rebuilt");
		assert!(
			with.late * 2 < without.late,
			"{} late with repairs, {} without",
			with.late,
			without.late
		);
	}
}

/// A path without loss delivers everything on time, and the repairs cost what was asked
#[test]
fn costs_its_overhead() {
	let scenario = Scenario {
		send: reed_solomon(20),
		..Scenario::default()
	};
	let with = run(&scenario);
	let without = run(&Scenario { send: None, ..scenario });
	assert_eq!(with.late, 0);
	assert_eq!(with.rebuilt, 0);
	let overhead = with.wire as f64 / without.wire as f64 - 1.0;
	info!(overhead, repairs = with.repairs, "overhead");
	assert!((0.15..0.30).contains(&overhead), "overhead {overhead}");
}

/// Fades that take whole bursts are rebuilt or retransmitted, and every byte still arrives
#[test]
fn survives_fades() {
	let scenario = Scenario {
		fades: Some(Fades {
			clear: Duration::from_millis(400),
			fade: Duration::from_millis(8),
			loss: 0.8,
		}),
		send: reed_solomon(30),
		controller: Controller::Bbr3,
		..Scenario::default()
	};
	let outcome = run(&scenario);
	info!(?outcome, "fades");
	assert_eq!(outcome.delays.len() as u64, outcome.frames);
	assert!(outcome.lost > 0);
}

/// A sender whose peer does not offer to rebuild sends no repairs
#[test]
fn off_unless_offered() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let mut client = client_config();
	client.transport.fec = Some(fec::Config::default());
	let (client_ch, server_ch) = pair.connect_with(client);
	let id = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let mut stream = pair.client_send(client_ch, id);
	while matches!(stream.try_write(&mut &[7; 4096][..]), Poll::Ready(Ok(_))) {}
	pair.drive();
	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.repair, 0);
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_rx.repair, 0);
}

/// One table of the sweep the experiment's write-up quotes, which `just rs quic-fec` prints
fn compare(rtt: u64, controller: Controller) {
	let single = |loss: f64| (format!("{:.0}% single", loss * 100.0), loss, None);
	let fades = |name: &str, clear: u64, fade: u64, loss: f64| {
		let fades = Fades {
			clear: Duration::from_millis(clear),
			fade: Duration::from_millis(fade),
			loss,
		};
		(name.to_string(), 0.0, Some(fades))
	};
	let links = [
		single(0.01),
		single(0.02),
		single(0.05),
		fades("2% in 2 ms fades", 98, 2, 1.0),
		fades("2% in 8 ms fades", 392, 8, 1.0),
		fades("wifi contended", 800, 16, 0.6),
		fades("cellular mobile", 3000, 24, 0.8),
	];
	let codes = [
		("none", None),
		("xor 8", xor(8)),
		("rs 10%", reed_solomon(10)),
		("rs 20%", reed_solomon(20)),
		("rs 30%", reed_solomon(30)),
	];
	let mut table = String::new();
	for (link, loss, fades) in &links {
		table += &format!("\nRTT {rtt} ms, {link}, {controller:?}: frame delay beyond the one-way delay, ms\n");
		table += &format!(
			"{:<8}{:>6}{:>7}{:>7}{:>7}{:>7}{:>8}{:>7}{:>9}\n",
			"code", "over", "p50", "p95", "p99", "max", "late", "lost", "rebuilt"
		);
		let mut base = 0;
		for (name, send) in &codes {
			let latency = Duration::from_millis(rtt / 2);
			let scenario = Scenario {
				latency,
				loss: *loss,
				fades: *fades,
				send: send.clone(),
				controller,
				duration: Duration::from_secs(300),
				..Scenario::default()
			};
			let outcome = run(&scenario);
			if send.is_none() {
				base = outcome.wire;
			}
			let ms = |p: f64| outcome.percentile(p).saturating_sub(latency).as_secs_f64() * 1e3;
			table += &format!(
				"{:<8}{:>5.0}%{:>7.1}{:>7.1}{:>7.1}{:>7.0}{:>7.2}%{:>6.2}%{:>9}\n",
				name,
				(outcome.wire as f64 / base as f64 - 1.0) * 100.0,
				ms(0.5),
				ms(0.95),
				ms(0.99),
				ms(1.0),
				outcome.late() * 100.0,
				outcome.lost as f64 / outcome.sent as f64 * 100.0,
				outcome.rebuilt,
			);
		}
	}
	// One write, so tables from tests running side by side do not interleave
	println!("{table}");
}

macro_rules! compare {
	($($name:ident: $rtt:expr, $controller:ident;)*) => {
		$(
			#[test]
			#[ignore = "a measurement, printed rather than asserted"]
			fn $name() {
				compare($rtt, Controller::$controller);
			}
		)*
	};
}

compare! {
	compare_20ms_bbr3: 20, Bbr3;
	compare_60ms_bbr3: 60, Bbr3;
	compare_150ms_bbr3: 150, Bbr3;
	compare_20ms_gcc: 20, Gcc;
	compare_60ms_gcc: 60, Gcc;
	compare_150ms_gcc: 150, Gcc;
	compare_20ms_unlimited: 20, Unlimited;
	compare_60ms_unlimited: 60, Unlimited;
	compare_150ms_unlimited: 150, Unlimited;
}

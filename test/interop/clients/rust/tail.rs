//! Finite raw-track interop client. The harness acknowledges a clean read on stdin.
//!
//! `drop` cuts group 1. `join` reads from whichever group the subscription starts at: a
//! moq-transport subscription starts at the largest group rather than at the range asked for,
//! so it must read a contiguous run of groups through the last one. `cached` publishes every
//! group before connecting, so a FETCH of a past group never races its write: moq-transport
//! refuses a FETCH that starts past the largest group rather than waiting for it. `compress`
//! publishes a compressed track with several frames, one empty, in each group. `late` writes
//! the head of group 0 before connecting and the rest only once the harness relays that the
//! subscriber read it, so a join that skips the head stalls rather than racing the publisher.
use std::time::Duration;

use anyhow::{Context, ensure};
use moq_tokio::moq_net;
use tokio::io::AsyncBufReadExt;

const GROUPS: u64 = 4;
const BYTES: usize = 256 * 1024;
/// The groups a `late` track publishes, and how many frames of group 0 precede the subscriber.
const LATE_GROUPS: u64 = 2;
const LATE_HEAD: usize = 2;

/// The frames of each group. A compressed track spans several frames, one empty, per window.
/// `cached` and `late` give every frame its own bytes, so a FETCH or join that drops, repeats,
/// or reorders one fails.
fn frames(sequence: u64, mode: Option<&str>) -> Vec<Vec<u8>> {
	match mode {
		Some("compress") => vec![
			vec![sequence as u8; BYTES / 2],
			Vec::new(),
			vec![sequence as u8; BYTES / 2],
		],
		Some("cached" | "late") => (0..3)
			.map(|index| vec![(sequence * 16 + index) as u8; BYTES / 4])
			.collect(),
		_ => vec![vec![sequence as u8; BYTES]],
	}
}

/// Publish every group, cutting group 1 for `drop`.
fn write(track: &mut moq_net::track::Producer, mode: Option<&str>) -> anyhow::Result<()> {
	// Declare the end before the final group's payload is even available.
	track.finish_at(moq_net::track::Position::group(GROUPS), None)?;
	for sequence in 0..GROUPS {
		let mut group = track.append_group()?;
		if mode == Some("drop") && sequence == 1 {
			group.abort(moq_net::Error::App(1))?;
			continue;
		}
		for frame in frames(sequence, mode) {
			group.write_frame(moq_net::Timestamp::ZERO, frame)?;
		}
		group.finish()?;
	}
	Ok(())
}

/// Read one whole group, checking every frame's bytes, and report when the `late` head is in.
async fn read(group: &mut moq_net::group::Consumer, mode: Option<&str>) -> anyhow::Result<()> {
	let sequence = group.sequence;
	for (index, want) in frames(sequence, mode).iter().enumerate() {
		let frame = group.read_frame().await?.context("short group")?;
		ensure!(frame.payload[..] == want[..], "wrong frame {index} in group {sequence}");
		if mode == Some("late") && sequence == 0 && index + 1 == LATE_HEAD {
			println!("late head");
		}
	}
	ensure!(group.read_frame().await?.is_none(), "extra frame in group {sequence}");
	Ok(())
}

// Keep metadata acceptance and alias registration in one task turn, before the
// subscriber acknowledges readiness to the harness.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(
		(4..=6).contains(&args.len()),
		"usage: interop-tail publish|subscribe URL BROADCAST [VERSION [drop|join|cached|compress|datagram|status|late]]"
	);
	let url: url::Url = args[2].parse()?;
	let mode = args.get(5).map(String::as_str);
	ensure!(
		matches!(
			mode,
			None | Some("drop" | "join" | "cached" | "compress" | "datagram" | "status" | "late")
		),
		"unknown mode: {mode:?}"
	);
	let drop = mode == Some("drop");
	let join = matches!(mode, Some("join" | "status"));
	let cached = mode == Some("cached");
	let compress = mode == Some("compress");
	let datagram = mode == Some("datagram");
	let status = mode == Some("status");
	let late = mode == Some("late");
	let mut commands = tokio::io::BufReader::new(tokio::io::stdin()).lines();
	let mut config = moq_tokio::connect::Config::default();
	// An empty version offers the defaults, so a mode can follow it.
	if let Some(version) = args.get(4).filter(|version| !version.is_empty()) {
		config.version.push(version.parse().map_err(anyhow::Error::msg)?);
	}
	config.websocket.enabled = Some(false);
	let client = config.init(Default::default())?.with_reconnect(false);
	let origin = moq_tokio::origin::spawn();

	match args[1].as_str() {
		"publish" => {
			let broadcast = origin.create_broadcast(&args[3])?;
			let mut track = broadcast.create_track(
				"tail",
				moq_net::track::Info::default()
					.with_timescale((!datagram).then_some(moq_net::Timescale::MILLI))
					.with_compress(compress),
			)?;
			broadcast.announce(Default::default())?;
			if cached {
				write(&mut track, mode)?;
			}
			let mut head = None;
			if late {
				let mut group = track.append_group()?;
				for frame in frames(0, mode).into_iter().take(LATE_HEAD) {
					group.write_frame(moq_net::Timestamp::ZERO, frame)?;
				}
				head = Some(group);
			}
			let session = client
				.clone()
				.with_publisher(&origin)
				.connect(url)
				.established()
				.await?;
			if let Some(mut group) = head {
				ensure!(
					commands.next_line().await?.as_deref() == Some("late tail"),
					"subscriber did not read the head"
				);
				for frame in frames(0, mode).into_iter().skip(LATE_HEAD) {
					group.write_frame(moq_net::Timestamp::ZERO, frame)?;
				}
				group.finish()?;
				for sequence in 1..LATE_GROUPS {
					let mut group = track.append_group()?;
					for frame in frames(sequence, mode) {
						group.write_frame(moq_net::Timestamp::ZERO, frame)?;
					}
					group.finish()?;
				}
				track.finish()?;
			} else if !cached {
				let mut demand = track.demand();
				while demand.wanted_changed().await?.is_none() {}
				if datagram {
					// Keep the track open until the harness acknowledges: a reliable end
					// must not overtake best-effort datagrams on their independent channel.
					for sequence in 0..GROUPS {
						ensure!(
							commands.next_line().await?.as_deref() == Some(&format!("datagram {sequence}")),
							"missing datagram command"
						);
						track.append_datagram(None, vec![sequence as u8; 900])?;
					}
				} else {
					write(&mut track, mode)?;
				}
			}
			// A finished track is not a drained transport. Keep the session alive
			// until the harness confirms the subscriber checked every byte and EOF.
			ensure!(
				commands.next_line().await?.as_deref() == Some("clean end"),
				"reader did not acknowledge a clean end"
			);
			if datagram {
				track.finish()?;
			}
			broadcast.close();
			session.close().await?;
			eprintln!("tail acknowledged");
		}
		"subscribe" => {
			let consumer = origin.consume();
			let session = client
				.clone()
				.with_subscriber(origin)
				.connect(url)
				.established()
				.await?;
			let broadcast = consumer.follow(&args[3], None)?.available().await?;
			let remote = broadcast.track("tail")?;
			if status {
				// Query before subscribing so draft-18+ must use TRACK_STATUS,
				// rather than learn metadata from SUBSCRIBE_OK as a side effect.
				let info = remote.info().await?;
				ensure!(info.timescale.is_none(), "IETF track should be untimed");
				ensure!(info.priority == 2, "TRACK_STATUS lost publisher priority");
				eprintln!("track status answered");
				session.close().await?;
				client.close().await;
				return Ok(());
			}
			if late {
				// No range: join the latest group as a player does, group 0, whose head predates
				// the subscription.
				let options = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(5));
				let mut track = remote.subscribe(Some(options)).await?;
				for sequence in 0..LATE_GROUPS {
					let mut group = track.recv_group().await?.context("missing group")?;
					ensure!(
						group.sequence == sequence,
						"joined group {} for {sequence}",
						group.sequence
					);
					read(&mut group, mode).await?;
				}
				ensure!(track.recv_group().await?.is_none(), "extra group");
				ensure!(
					track.finished().await? == moq_net::track::Position::group(LATE_GROUPS),
					"wrong declared end"
				);
				eprintln!("late join clean groups=0,1 frames=6");
				session.close().await?;
				client.close().await;
				return Ok(());
			}
			let options = moq_net::track::Subscription::default()
				.with_max_delay(Duration::from_secs(if drop { 60 } else { 5 }))
				.with_range(0..);
			let mut track = remote.subscribe(Some(options)).await?;
			if datagram {
				println!("datagram ready");
				for sequence in 0..GROUPS {
					let frame = track.recv_datagram().await?.context("missing datagram")?;
					ensure!(frame.sequence == sequence, "wrong datagram sequence");
					ensure!(frame.payload.len() == 900, "truncated datagram");
					ensure!(
						frame.payload.iter().all(|byte| *byte == sequence as u8),
						"corrupt datagram"
					);
					ensure!(frame.timestamp.is_none(), "unexpected IETF timestamp");
					println!("datagram {sequence}");
				}
				eprintln!("datagram clean groups=0,1,2,3 bytes=3600");
				session.close().await?;
				client.close().await;
				return Ok(());
			}
			if compress {
				ensure!(
					track.info().compress == args.get(4).is_some_and(|version| version == "moq-lite-07-wip"),
					"wrong compression metadata"
				);
			}
			let mut seen = Vec::new();
			while let Some(mut group) = track.recv_group().await? {
				let sequence = group.sequence;
				ensure!(sequence < GROUPS, "unexpected group {sequence}");
				read(&mut group, mode).await?;
				seen.push(sequence);
			}
			seen.sort_unstable();
			let first = if join { seen.first().copied().unwrap_or(0) } else { 0 };
			let expected: Vec<u64> = if drop { vec![0, 2, 3] } else { (first..GROUPS).collect() };
			ensure!(seen == expected, "wrong groups: {seen:?}");
			ensure!(
				track.finished().await? == moq_net::track::Position::group(GROUPS),
				"wrong declared end"
			);
			session.close().await?;
			if join {
				let groups: Vec<_> = seen.iter().map(u64::to_string).collect();
				eprintln!("tail join clean end=4 groups={}", groups.join(","));
			} else if drop {
				eprintln!("tail drop clean end=4 groups=0,2,3 bytes=786432");
			} else {
				eprintln!("tail clean end=4 groups=0,1,2,3 bytes=1048576");
			}
		}
		_ => anyhow::bail!("unknown role: {}", args[1]),
	}
	// Dropping only queues the QUIC close; send it before the runtime stops.
	client.close().await;
	Ok(())
}

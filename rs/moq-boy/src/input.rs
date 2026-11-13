//! Viewer sessions and their commands.
//!
//! The emulator runs a server socket endpoint named for its game, and each viewer dials it
//! from a client endpoint in a `track` session. Each group the viewer writes is one JSON command: button states or a
//! reset. The emulator answers each button command with that viewer's latency breakdown,
//! one group per reply, on its own half of the session. A viewer is gone once its session
//! ends, and a reload is a new session.

use std::time::Duration;

use crate::emulator::Button;
use crate::status::LatencyEntry;

/// A single timestamp entry from the viewer.
#[derive(serde::Deserialize, Debug)]
struct RawTimestamp {
	label: String,
	/// Media timestamp in milliseconds.
	ts: f64,
}

/// A parsed timestamp entry with Duration.
#[derive(Debug)]
pub struct TimestampEntry {
	pub label: String,
	pub ts: Duration,
}

/// A command sent by a viewer.
#[derive(serde::Deserialize, Debug)]
#[serde(tag = "type")]
enum RawCommand {
	#[serde(rename = "buttons")]
	Buttons {
		#[serde(default)]
		buttons: Vec<Button>,
		/// Ordered media timestamps at each pipeline stage.
		#[serde(default)]
		timestamps: Vec<RawTimestamp>,
	},
	#[serde(rename = "reset")]
	Reset {},
}

/// What the emulator writes back to one viewer, one per group.
#[derive(serde::Serialize)]
struct Reply<'a> {
	/// The viewer's latency breakdown, in pipeline order.
	latency: &'a [LatencyEntry],
}

/// One viewer's session, numbered locally: the session id is a secret, so it stays out
/// of logs and status.
pub type ViewerId = u64;

/// Where the emulator writes a viewer's replies.
pub struct Replies(moq_net::track::Producer);

impl Replies {
	/// Send the viewer its latency breakdown.
	pub fn latency(&mut self, latency: &[LatencyEntry]) {
		let reply = serde_json::to_vec(&Reply { latency }).expect("a reply always serializes");
		if let Err(err) = send(&self.0, reply) {
			tracing::debug!(%err, "viewer reply dropped");
		}
	}
}

/// Send `message` on a session track as a group of its own.
///
/// A session track carries no timescale, so its groups have no time to start at: each starts one
/// past the newest the track holds.
fn send(track: &moq_net::track::Producer, message: Vec<u8>) -> moq_net::Result<()> {
	let mut group = track.append_group(track.largest().map_or(0, |largest| largest.group + 1))?;
	group.write_frame(None, message)?;
	group.close()?;
	Ok(())
}

/// A viewer's session event, in the order the emulator applies them.
pub enum Command {
	/// A viewer's session opened.
	Joined {
		viewer: ViewerId,
		replies: Replies,
	},
	/// Full button state for a viewer.
	Buttons {
		viewer: ViewerId,
		buttons: Vec<Button>,
		/// Ordered media timestamps at each pipeline stage.
		timestamps: Vec<TimestampEntry>,
	},
	Reset,
	/// A viewer's session ended.
	Left {
		viewer: ViewerId,
	},
}

/// Accept every viewer that dials `endpoint`, forwarding their commands to the emulator.
///
/// Fails once the endpoint can no longer accept: it closed, or a hop to the relay can't
/// carry sockets.
pub async fn handle_viewers(
	endpoint: &moq_net::socket::Endpoint,
	cmd_tx: &tokio::sync::mpsc::Sender<Command>,
) -> anyhow::Result<()> {
	for viewer in 0.. {
		let incoming = endpoint.accept_track().await?;
		tokio::spawn(run_viewer(viewer, incoming, cmd_tx.clone()));
	}
	unreachable!("viewer ids are exhausted")
}

/// Accept one viewer's dial and relay its commands until the session ends.
async fn run_viewer(
	viewer: ViewerId,
	incoming: moq_net::socket::Incoming<moq_net::socket::TrackStream>,
	cmd_tx: tokio::sync::mpsc::Sender<Command>,
) {
	let peer = incoming.peer().to_string();
	let stream = match incoming.accept(None).await {
		Ok(stream) => stream,
		Err(err) => {
			tracing::debug!(%peer, %err, "viewer dial failed");
			return;
		}
	};
	tracing::info!(%peer, viewer, "viewer connected");

	let replies = Replies(stream.producer.clone());
	if cmd_tx.send(Command::Joined { viewer, replies }).await.is_err() {
		return;
	}

	let result = read_commands(viewer, stream.consumer, &cmd_tx).await;
	let _ = cmd_tx.send(Command::Left { viewer }).await;
	match result {
		Ok(()) => {
			tracing::info!(%peer, viewer, "viewer disconnected");
			let _ = stream.producer.finish();
		}
		Err(err) => {
			tracing::info!(%peer, viewer, %err, "viewer session failed");
			let _ = stream.producer.abort(err);
		}
	}
}

/// Forward each command the viewer writes, one per group, until it finishes its half.
async fn read_commands(
	viewer: ViewerId,
	consumer: moq_net::track::Subscriber,
	cmd_tx: &tokio::sync::mpsc::Sender<Command>,
) -> Result<(), moq_net::Error> {
	// Each command carries the viewer's whole button state, so a newer one supersedes a
	// delayed one rather than waiting behind it. A reset can be skipped the same way, only
	// when a newer command overtakes it in flight.
	let live = consumer.subscription().with_delay(moq_net::track::Delay::default());
	consumer.control().update(live)?;
	let mut commands = consumer.ordered();
	while let Some(mut group) = commands.next_group().await? {
		let frame = match group.read_frame().await {
			Ok(Some(frame)) => frame,
			Ok(None) => continue,
			// Judged by stream code, so a group the viewer's side reset is the same loss as
			// one the delay budget dropped here. A newer command restates the whole state.
			Err(err)
				if matches!(
					moq_net::StreamError::from(&err),
					moq_net::StreamError::Old
						| moq_net::StreamError::Evicted
						| moq_net::StreamError::TooFarBehind
						| moq_net::StreamError::GroupTooLarge
				) =>
			{
				tracing::debug!(viewer, %err, "command lost");
				continue;
			}
			Err(err) => return Err(err),
		};

		// A viewer is untrusted: a malformed command is skipped, not fatal.
		let command = match serde_json::from_slice::<RawCommand>(&frame.payload) {
			Ok(command) => command,
			Err(err) => {
				tracing::warn!(viewer, %err, "invalid command");
				continue;
			}
		};

		let command = match command {
			RawCommand::Buttons { buttons, timestamps } => {
				let timestamps = timestamps
					.into_iter()
					.filter_map(|t| {
						let ts = Duration::try_from_secs_f64(t.ts / 1000.0).ok()?;
						Some(TimestampEntry { label: t.label, ts })
					})
					.collect();
				Command::Buttons {
					viewer,
					buttons,
					timestamps,
				}
			}
			RawCommand::Reset {} => {
				tracing::info!(viewer, "reset");
				Command::Reset
			}
		};
		if cmd_tx.send(command).await.is_err() {
			break;
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use moq_net::socket::{Endpoint, TrackStream};

	use super::*;

	/// An emulator endpoint and a viewer endpoint on one origin, the emulator accepting.
	async fn setup() -> (
		moq_net::origin::Producer,
		Endpoint,
		TrackStream,
		tokio::sync::mpsc::Receiver<Command>,
	) {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let game = Endpoint::new(&origin, "boy", "tetris").unwrap();
		let (tx, rx) = tokio::sync::mpsc::channel(16);
		tokio::spawn(async move { handle_viewers(&game, &tx).await });

		let viewer = Endpoint::new(&origin, "boy", "alice").unwrap();
		let session = kio::clock::timeout(Duration::from_secs(5), viewer.dial_track("tetris", None).unwrap())
			.await
			.expect("dial timeout")
			.expect("dial");
		(origin, viewer, session, rx)
	}

	fn send(session: &mut TrackStream, command: &str) {
		super::send(&session.producer, command.as_bytes().to_vec()).unwrap();
	}

	async fn recv(rx: &mut tokio::sync::mpsc::Receiver<Command>) -> Command {
		kio::clock::timeout(Duration::from_secs(5), rx.recv())
			.await
			.expect("recv timeout")
			.expect("handler ended")
	}

	/// A viewer is untrusted: a malformed command, or a timestamp no Duration can
	/// hold, is dropped without ending that viewer's session.
	#[tokio::test(start_paused = true)]
	async fn malformed_commands_are_skipped() {
		let (_origin, _viewer, mut session, mut rx) = setup().await;
		let Command::Joined { viewer, .. } = recv(&mut rx).await else {
			panic!("the session did not join first");
		};

		send(&mut session, "not json");
		send(&mut session, r#"{"type":"explode"}"#);
		send(&mut session, r#"{"type":"buttons","buttons":["turbo"]}"#);
		send(
			&mut session,
			r#"{"type":"buttons","buttons":["a"],"timestamps":[{"label":"past","ts":-5},{"label":"far","ts":1e300},{"label":"ok","ts":1500}]}"#,
		);

		let Command::Buttons {
			viewer: from,
			buttons,
			timestamps,
		} = recv(&mut rx).await
		else {
			panic!("the viewer's session ended at a malformed command");
		};
		assert_eq!(from, viewer);
		assert_eq!(buttons, vec![Button::A]);
		assert_eq!(timestamps.len(), 1, "{timestamps:?}");
		assert_eq!(timestamps[0].label, "ok");
		assert_eq!(timestamps[0].ts, Duration::from_millis(1500));
	}

	/// A command still in flight when a newer one lands is superseded, not a session
	/// failure: the newer state reaches the emulator on the same session.
	#[tokio::test(start_paused = true)]
	async fn a_superseded_command_is_skipped() {
		let (_origin, _viewer, mut session, mut rx) = setup().await;
		let Command::Joined { viewer, .. } = recv(&mut rx).await else {
			panic!("the session did not join first");
		};

		// The reader parks on this group's first frame, which never arrives.
		let _stalled = session
			.producer
			.append_group(session.producer.largest().map_or(0, |largest| largest.group + 1))
			.unwrap();
		tokio::task::yield_now().await;
		send(&mut session, r#"{"type":"buttons","buttons":["a"]}"#);

		let Command::Buttons {
			viewer: from, buttons, ..
		} = recv(&mut rx).await
		else {
			panic!("the superseded command ended the session");
		};
		assert_eq!(from, viewer);
		assert_eq!(buttons, vec![Button::A]);
	}

	/// Replies reach the viewer on its own session, and finishing the viewer's half
	/// counts it as gone.
	#[tokio::test(start_paused = true)]
	async fn replies_reach_the_viewer_and_a_finish_leaves() {
		let (_origin, _viewer, session, mut rx) = setup().await;
		let Command::Joined { viewer, mut replies } = recv(&mut rx).await else {
			panic!("the session did not join first");
		};

		replies.latency(&[LatencyEntry {
			label: "input".into(),
			ms: 42,
		}]);
		let mut reader = session.consumer.ordered();
		let mut group = kio::clock::timeout(Duration::from_secs(5), reader.next_group())
			.await
			.expect("reply timeout")
			.unwrap()
			.expect("a reply");
		let frame = group.read_frame().await.unwrap().expect("a frame");
		assert_eq!(&frame.payload[..], br#"{"latency":[{"label":"input","ms":42}]}"#);

		let _producer = session.producer.finish().unwrap();
		let Command::Left { viewer: left } = recv(&mut rx).await else {
			panic!("the finished session did not leave");
		};
		assert_eq!(left, viewer);
	}

	/// Each session is its own viewer, so a reload's new session never merges with the old.
	#[tokio::test(start_paused = true)]
	async fn each_session_is_a_viewer() {
		let (origin, viewer, first, mut rx) = setup().await;
		let Command::Joined { viewer: a, .. } = recv(&mut rx).await else {
			panic!("the first session did not join");
		};

		let second = kio::clock::timeout(Duration::from_secs(5), viewer.dial_track("tetris", None).unwrap())
			.await
			.expect("dial timeout")
			.expect("dial");
		let Command::Joined { viewer: b, .. } = recv(&mut rx).await else {
			panic!("the second session did not join");
		};
		assert_ne!(a, b);

		drop(first);
		let Command::Left { viewer: left } = recv(&mut rx).await else {
			panic!("the dropped session did not leave");
		};
		assert_eq!(left, a);
		drop((second, origin));
	}
}

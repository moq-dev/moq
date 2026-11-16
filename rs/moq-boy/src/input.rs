//! Viewer discovery and command handling.
//!
//! Viewers are MoQ publishers: each creates a broadcast under the viewer prefix
//! with a "command" track containing JSON button states and reset requests. The
//! command track is read through [`moq_json`], so snapshots and merge-patch
//! deltas are reconstructed into a full command before dispatch. This module
//! discovers viewer broadcasts and relays their commands to the emulator thread
//! via an mpsc channel.

use std::time::Duration;

use crate::emulator::Button;

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

/// A viewer's broadcast path under the game's viewer prefix, which keys its buttons and latency.
pub type ViewerId = String;

/// A command with viewer identity attached.
#[derive(Debug)]
pub enum Command {
	/// Full button state for a viewer.
	Buttons {
		viewer: ViewerId,
		buttons: Vec<Button>,
		/// Ordered media timestamps at each pipeline stage.
		timestamps: Vec<TimestampEntry>,
	},
	Reset,
	/// A viewer stopped publishing, went offline, or sent a frame that doesn't decode.
	Left {
		viewer: ViewerId,
	},
}

/// Read the commands of every viewer broadcast announced in `viewers`.
pub async fn handle_viewers(
	viewers: &moq_net::origin::Consumer,
	cmd_tx: &tokio::sync::mpsc::Sender<Command>,
) -> anyhow::Result<()> {
	let mut announced = viewers.announced();
	// The follower per viewer, which reads each broadcast a restarted viewer replaces the old with.
	let mut readers: std::collections::HashMap<ViewerId, tokio::task::AbortHandle> = Default::default();
	while let Some(event) = announced.next().await {
		match event {
			moq_net::announce::Event::Present(announce) => {
				let viewer = announce.prefix.to_string();
				let reader = tokio::spawn(follow_viewer(viewers.clone(), announce.prefix, cmd_tx.clone()));
				if let Some(old) = readers.insert(viewer, reader.abort_handle()) {
					old.abort();
				}
			}
			moq_net::announce::Event::Absent(announce) | moq_net::announce::Event::Offline(announce) => {
				let viewer = announce.prefix.to_string();
				// Dropping an abort handle leaves its task running, so stop it here.
				if let Some(old) = readers.remove(&viewer) {
					old.abort();
				}
				tracing::info!(%viewer, "viewer went offline");
				let _ = cmd_tx.send(Command::Left { viewer }).await;
			}
			// The follower resumes on its own once the path is reachable again.
			moq_net::announce::Event::Unavailable(_) | moq_net::announce::Event::Available(_) => {}
		}
	}
	anyhow::bail!("viewer origin closed")
}

/// Read the commands of each broadcast a viewer publishes at `path`, switching to the new one
/// when the viewer restarts. A refused resolution waits for the serving route to change, since
/// the listing reports nothing new while the path stays present.
async fn follow_viewer(
	origin: moq_net::origin::Consumer,
	path: moq_net::Path,
	cmd_tx: tokio::sync::mpsc::Sender<Command>,
) {
	let viewer = path.to_string();
	let Ok(mut following) = origin.follow(&path, None) else {
		return;
	};
	let mut reading = None;
	loop {
		tokio::select! {
			change = following.available() => match change {
				Ok(Some(broadcast)) => {
					tracing::info!(%viewer, "viewer connected");
					reading = Some(Box::pin(read_commands(&viewer, broadcast, &cmd_tx)));
				}
				Ok(None) => {}
				// The follower's documented end: it returns Closed on every call once the origin
				// closes, so waiting on it again would spin.
				Err(moq_net::Error::Closed) => return,
				// A refused resolution, which the next call retries once the serving route changes.
				Err(err) => tracing::debug!(%viewer, %err, "viewer not resolved; waiting for a route change"),
			},
			read = async { reading.as_mut().expect("guarded").await }, if reading.is_some() => {
				reading = None;
				if let Err(err) = read {
					tracing::warn!(%viewer, %err, "viewer command error");
				}
				tracing::info!(%viewer, "viewer disconnected");
				let _ = cmd_tx.send(Command::Left { viewer: viewer.clone() }).await;
			}
		}
	}
}

/// Forward the newest command of one viewer broadcast until its track ends or fails.
///
/// A command that decodes as JSON but isn't one the emulator knows is skipped, so a newer viewer
/// can add commands. A frame that isn't JSON, or a merge patch that doesn't apply, ends the read:
/// that viewer's publisher is broken, and it hurts only that viewer.
async fn read_commands(
	viewer: &str,
	broadcast: moq_net::broadcast::Consumer,
	cmd_tx: &tokio::sync::mpsc::Sender<Command>,
) -> anyhow::Result<()> {
	// The consumer subscribes at a zero delay budget and skips a lost group, so a command still in
	// flight never holds up a newer one: each restates the viewer's whole button state.
	let mut commands = moq_json::snapshot::Consumer::<serde_json::Value>::new(
		broadcast.track("command")?,
		moq_json::snapshot::consumer::Config::default(),
	);

	while let Some(command) = commands.latest().await? {
		let command = match serde_json::from_value::<RawCommand>(command.value) {
			Ok(command) => command,
			Err(err) => {
				tracing::warn!(%viewer, %err, "unknown command");
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
					viewer: viewer.to_string(),
					buttons,
					timestamps,
				}
			}
			RawCommand::Reset {} => {
				tracing::info!(%viewer, "reset");
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

	use super::*;

	/// An origin whose viewer broadcasts the handler reads, as the emulator scopes it.
	fn setup() -> (moq_net::origin::Producer, tokio::sync::mpsc::Receiver<Command>) {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let (tx, rx) = tokio::sync::mpsc::channel(16);
		let consumer = origin.consume();
		tokio::spawn(async move { handle_viewers(&consumer, &tx).await });
		(origin, rx)
	}

	/// A viewer's broadcast and its command track, as the element publishes them.
	struct Viewer {
		route: moq_net::origin::Route,
		track: moq_net::track::Producer,
		commands: moq_json::snapshot::Producer<serde_json::Value>,
		at: u64,
	}

	impl Viewer {
		fn new(origin: &moq_net::origin::Producer, id: &str) -> Self {
			let route = origin.route(id, moq_net::origin::RouteOptions::default()).unwrap();
			let track = route
				.broadcast()
				.create_track(
					"command",
					moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
				)
				.unwrap();
			let commands = moq_json::snapshot::Producer::new(track.clone(), moq_json::snapshot::Config::default());
			Self {
				route,
				track,
				commands,
				at: 0,
			}
		}

		/// Publish a command, a snapshot or a delta against the last one.
		fn send(&mut self, command: &str) {
			self.at += 1;
			let value: serde_json::Value = serde_json::from_str(command).unwrap();
			let at = moq_net::Timestamp::from_millis(self.at).unwrap();
			self.commands.update(moq_json::Timed::new(&value, at)).unwrap();
		}
	}

	async fn recv(rx: &mut tokio::sync::mpsc::Receiver<Command>) -> Command {
		kio::clock::timeout(Duration::from_secs(5), rx.recv())
			.await
			.expect("recv timeout")
			.expect("handler ended")
	}

	/// A viewer is untrusted: an unknown command, or a timestamp no Duration can hold, is dropped
	/// without ending that viewer's commands.
	#[tokio::test(start_paused = true)]
	async fn unknown_commands_are_skipped() {
		let (origin, mut rx) = setup();
		let mut viewer = Viewer::new(&origin, "alice");

		// Read one at a time, so each is decoded rather than superseded by the next.
		for command in [r#"{"type":"explode"}"#, r#"{"type":"buttons","buttons":["turbo"]}"#] {
			viewer.send(command);
			kio::clock::sleep(Duration::from_millis(100)).await;
		}
		viewer.send(
			r#"{"type":"buttons","buttons":["a"],"timestamps":[{"label":"past","ts":-5},{"label":"far","ts":1e300},{"label":"ok","ts":1500}]}"#,
		);

		let Command::Buttons {
			viewer: from,
			buttons,
			timestamps,
		} = recv(&mut rx).await
		else {
			panic!("the viewer's commands ended at an unknown command");
		};
		assert_eq!(from, "alice");
		assert_eq!(buttons, vec![Button::A]);
		assert_eq!(timestamps.len(), 1, "{timestamps:?}");
		assert_eq!(timestamps[0].label, "ok");
		assert_eq!(timestamps[0].ts, Duration::from_millis(1500));
	}

	/// A frame that isn't JSON ends only its own viewer: the other keeps playing.
	#[tokio::test(start_paused = true)]
	async fn an_undecodable_frame_ends_only_its_viewer() {
		let (origin, mut rx) = setup();
		let broken = Viewer::new(&origin, "mallory");
		let mut alice = Viewer::new(&origin, "alice");
		kio::clock::sleep(Duration::from_millis(100)).await;

		let mut group = broken.track.append_group(1).unwrap();
		group
			.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"not json".to_vec())
			.unwrap();
		group.close().unwrap();
		let Command::Left { viewer } = recv(&mut rx).await else {
			panic!("the broken viewer did not leave");
		};
		assert_eq!(viewer, "mallory");

		alice.send(r#"{"type":"buttons","buttons":["b"]}"#);
		let Command::Buttons { viewer, buttons, .. } = recv(&mut rx).await else {
			panic!("another viewer's frame ended this one");
		};
		assert_eq!(viewer, "alice");
		assert_eq!(buttons, vec![Button::B]);
		drop(broken);
	}

	/// A command still in flight when a newer one lands is superseded, not a failure: the newer
	/// state reaches the emulator.
	#[tokio::test(start_paused = true)]
	async fn a_superseded_command_is_skipped() {
		let (origin, mut rx) = setup();
		let mut viewer = Viewer::new(&origin, "alice");
		kio::clock::sleep(Duration::from_millis(100)).await;

		// The reader parks on this group's first frame, which never arrives.
		let _stalled = viewer.track.append_group(1).unwrap();
		tokio::task::yield_now().await;
		viewer.at = 1;
		viewer.send(r#"{"type":"buttons","buttons":["a"]}"#);

		let Command::Buttons {
			viewer: from, buttons, ..
		} = recv(&mut rx).await
		else {
			panic!("the superseded command ended the viewer");
		};
		assert_eq!(from, "alice");
		assert_eq!(buttons, vec![Button::A]);
	}

	/// Each broadcast is its own viewer, and withdrawing one leaves only that one.
	#[tokio::test(start_paused = true)]
	async fn each_broadcast_is_a_viewer_until_it_leaves() {
		let (origin, mut rx) = setup();
		let mut alice = Viewer::new(&origin, "alice");
		let mut bob = Viewer::new(&origin, "bob");
		kio::clock::sleep(Duration::from_millis(100)).await;

		alice.send(r#"{"type":"buttons","buttons":["a"]}"#);
		let Command::Buttons { viewer, .. } = recv(&mut rx).await else {
			panic!("alice's command did not arrive");
		};
		assert_eq!(viewer, "alice");
		bob.send(r#"{"type":"buttons","buttons":["b"]}"#);
		let Command::Buttons { viewer, .. } = recv(&mut rx).await else {
			panic!("bob's command did not arrive");
		};
		assert_eq!(viewer, "bob");

		alice.route.withdraw();
		let Command::Left { viewer } = recv(&mut rx).await else {
			panic!("the withdrawn viewer did not leave");
		};
		assert_eq!(viewer, "alice");

		bob.send(r#"{"type":"reset"}"#);
		assert!(matches!(recv(&mut rx).await, Command::Reset));
	}

	/// A viewer whose handler refuses it stays followed: the listing reports nothing new when a
	/// publisher takes the path over, so only the follower can pick it up.
	#[tokio::test(start_paused = true)]
	async fn a_refused_viewer_is_read_once_a_publisher_takes_over() {
		let (origin, mut rx) = setup();

		let claim = origin.claim("alice", moq_net::origin::RouteOptions::default()).unwrap();
		kio::clock::timeout(Duration::from_secs(1), claim.requested())
			.await
			.expect("never asked")
			.expect("the claim closed")
			.reject(moq_net::Error::NotFound);
		kio::clock::sleep(Duration::from_secs(1)).await;

		let mut viewer = Viewer::new(&origin, "alice");
		kio::clock::sleep(Duration::from_secs(1)).await;
		viewer.send(r#"{"type":"reset"}"#);
		assert!(matches!(recv(&mut rx).await, Command::Reset));
	}
}

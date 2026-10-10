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

/// A command with viewer identity attached.
#[derive(Debug)]
pub enum Command {
	/// Full button state for a viewer.
	Buttons {
		buttons: Vec<Button>,
		viewer_id: String,
		/// Ordered media timestamps at each pipeline stage.
		timestamps: Vec<TimestampEntry>,
	},
	Reset,
	/// A viewer disconnected or went offline.
	ViewerLeft {
		viewer_id: String,
	},
}

/// Handles discovered viewers: subscribes to their command tracks.
pub async fn handle_viewers(
	viewer_origin: &moq_net::origin::Consumer,
	cmd_tx: &tokio::sync::mpsc::Sender<Command>,
) -> anyhow::Result<()> {
	let mut announced = viewer_origin.announced();
	// The command reader per viewer, so a restarted viewer replaces the old one's.
	let mut readers: std::collections::HashMap<String, tokio::task::AbortHandle> = Default::default();
	loop {
		let (update, active, restart) = match announced.next().await {
			Some(moq_net::announce::Event::Start(update) | moq_net::announce::Event::Update(update)) => {
				(update, true, false)
			}
			// The viewer restarted: read the new broadcast instead of the old one.
			Some(moq_net::announce::Event::Restart(update)) => (update, true, true),
			Some(moq_net::announce::Event::End(update)) => (update, false, false),
			None => break,
		};

		let viewer_id = update.prefix.to_string();

		if active {
			if !restart && readers.get(&viewer_id).is_some_and(|reader| !reader.is_finished()) {
				continue;
			}
			let Ok(broadcast) = viewer_origin.request_broadcast(&update.prefix, None).await else {
				continue;
			};
			tracing::info!(%viewer_id, restart, "viewer connected");
			if let Some(old) = readers.remove(&viewer_id) {
				old.abort();
			}
			let cmd_tx = cmd_tx.clone();
			let vid = viewer_id.clone();
			let reader = tokio::spawn(async move {
				if let Err(e) = handle_viewer_commands(&vid, broadcast, &cmd_tx).await {
					tracing::warn!(viewer_id = %vid, error = %e, "viewer command error");
				}
				tracing::info!(viewer_id = %vid, "viewer disconnected");
				let _ = cmd_tx.send(Command::ViewerLeft { viewer_id: vid }).await;
			});
			readers.insert(viewer_id, reader.abort_handle());
		} else {
			// Dropping an abort handle leaves its task running. Older versions send a restart
			// as END then START, and the old track keeps flowing meanwhile, so the old reader
			// must stop here or it would race the replacement.
			if let Some(old) = readers.remove(&viewer_id) {
				old.abort();
			}
			tracing::info!(%viewer_id, "viewer went offline");
			let _ = cmd_tx
				.send(Command::ViewerLeft {
					viewer_id: viewer_id.clone(),
				})
				.await;
		}
	}
	Ok(())
}

async fn handle_viewer_commands(
	viewer_id: &str,
	broadcast: moq_net::broadcast::Consumer,
	cmd_tx: &tokio::sync::mpsc::Sender<Command>,
) -> anyhow::Result<()> {
	let track = broadcast.track("command")?.subscribe(None).await?;
	let mut commands =
		moq_json::snapshot::Consumer::<RawCommand>::new(track, moq_json::snapshot::consumer::Config::default());

	loop {
		let command = match commands.next().await {
			Ok(Some(command)) => command,
			Ok(None) => break,
			// A malformed command is skipped; a track error ends the stream.
			Err(moq_json::Error::Json(err)) => {
				tracing::warn!(%viewer_id, %err, "invalid command");
				continue;
			}
			Err(err) => return Err(err.into()),
		};

		match command {
			RawCommand::Buttons { buttons, timestamps } => {
				let timestamps: Vec<_> = timestamps
					.into_iter()
					.filter_map(|t| {
						let ts = Duration::try_from_secs_f64(t.ts / 1000.0).ok()?;
						Some(TimestampEntry { label: t.label, ts })
					})
					.collect();
				let _ = cmd_tx
					.send(Command::Buttons {
						buttons,
						viewer_id: viewer_id.to_string(),
						timestamps,
					})
					.await;
			}
			RawCommand::Reset {} => {
				tracing::info!(%viewer_id, "reset");
				let _ = cmd_tx.send(Command::Reset).await;
			}
		}
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	/// Write one command to the track as its own group.
	fn send(track: &moq_net::track::Producer, command: &str) {
		let mut group = track.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, command.as_bytes().to_vec())
			.unwrap();
		group.finish().unwrap();
	}

	/// Publish a viewer with a live command track.
	fn viewer(origin: &moq_net::origin::Producer) -> (moq_net::broadcast::Producer, moq_net::track::Producer) {
		let broadcast = origin.create_broadcast("alice").unwrap();
		let track = broadcast.create_track("command", None).unwrap();
		broadcast.announce(moq_net::origin::Route::default()).unwrap();
		(broadcast, track)
	}

	/// Older versions send a restart as END then START while the old command track
	/// keeps flowing: the old reader stops at the END, so it neither forwards commands
	/// nor reports the replacement gone.
	#[tokio::test(start_paused = true)]
	async fn end_then_start_replaces_the_reader() {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let (tx, mut rx) = tokio::sync::mpsc::channel(16);
		let consumer = origin.consume();
		tokio::spawn(async move { handle_viewers(&consumer, &tx).await });

		let (old, old_track) = viewer(&origin);
		tokio::time::sleep(Duration::from_secs(1)).await;
		send(&old_track, r#"{"type":"reset"}"#);
		assert!(matches!(rx.recv().await, Some(Command::Reset)));

		old.unannounce();
		assert!(matches!(rx.recv().await, Some(Command::ViewerLeft { .. })));
		let (_new, new_track) = viewer(&origin);
		tokio::time::sleep(Duration::from_secs(1)).await;

		// The old track is still live: anything it says now comes from a stale reader.
		send(&old_track, r#"{"type":"reset"}"#);
		drop(old_track);
		drop(old);
		tokio::time::sleep(Duration::from_secs(1)).await;

		send(&new_track, r#"{"type":"buttons","buttons":[]}"#);
		let next = rx.recv().await;
		assert!(matches!(next, Some(Command::Buttons { .. })), "{next:?}");
		tokio::time::sleep(Duration::from_secs(1)).await;
		assert!(rx.try_recv().is_err(), "the old reader is still running");
	}
}

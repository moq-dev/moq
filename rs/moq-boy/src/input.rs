//! Viewer discovery and command handling.
//!
//! Viewers are MoQ publishers: each creates a broadcast under the viewer prefix
//! with a "command" track containing JSON button states and reset requests.
//! This module discovers viewer broadcasts and relays their commands to the
//! emulator thread via an mpsc channel.

use anyhow::Context;

use std::time::Duration;

use crate::emulator::Button;

/// A command sent by a viewer.
#[derive(serde::Deserialize, Debug)]
#[serde(tag = "type")]
enum RawCommand {
	#[serde(rename = "buttons")]
	Buttons {
		#[serde(default)]
		buttons: Vec<Button>,
		/// The viewer's current media timestamp in milliseconds (from JSON).
		#[serde(default)]
		ts: f64,
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
		/// The viewer's current media timestamp.
		ts: Duration,
	},
	Reset,
	/// A viewer disconnected or went offline.
	ViewerLeft {
		viewer_id: String,
	},
}

/// Handles discovered viewers: subscribes to their command tracks.
pub async fn handle_viewers(
	viewer_origin: &mut moq_lite::OriginConsumer,
	cmd_tx: &tokio::sync::mpsc::Sender<Command>,
) -> anyhow::Result<()> {
	loop {
		let Some((path, broadcast)) = viewer_origin.announced().await else {
			break;
		};

		let viewer_id = path.to_string();

		if let Some(broadcast) = broadcast {
			tracing::info!(%viewer_id, "viewer connected");
			let cmd_tx = cmd_tx.clone();
			let vid = viewer_id.clone();
			tokio::spawn(async move {
				if let Err(e) = handle_viewer_commands(&vid, broadcast, &cmd_tx).await {
					tracing::warn!(viewer_id = %vid, error = %e, "viewer command error");
				}
				tracing::info!(viewer_id = %vid, "viewer disconnected");
				let _ = cmd_tx.send(Command::ViewerLeft { viewer_id: vid }).await;
			});
		} else {
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
	broadcast: moq_lite::BroadcastConsumer,
	cmd_tx: &tokio::sync::mpsc::Sender<Command>,
) -> anyhow::Result<()> {
	let command_track = moq_lite::Track {
		name: "command".to_string(),
		priority: 0,
	};

	let mut track = broadcast.subscribe_track(&command_track)?;

	while let Some(mut group) = track.next_group().await? {
		while let Some(frame) = group.read_frame().await? {
			let text = std::str::from_utf8(&frame).context("invalid UTF-8 in command")?;
			match serde_json::from_str::<RawCommand>(text) {
				Ok(RawCommand::Buttons { buttons, ts }) => {
					let _ = cmd_tx
						.send(Command::Buttons {
							buttons,
							viewer_id: viewer_id.to_string(),
							ts: Duration::from_secs_f64(ts / 1000.0),
						})
						.await;
				}
				Ok(RawCommand::Reset { .. }) => {
					tracing::info!(%viewer_id, "reset");
					let _ = cmd_tx.send(Command::Reset).await;
				}
				Err(e) => {
					tracing::warn!(%viewer_id, error = %e, "invalid command");
				}
			}
		}
	}

	Ok(())
}

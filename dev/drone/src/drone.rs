use std::sync::{Arc, Mutex};

use anyhow::Context;

use crate::Config;
use crate::game;
use crate::sensor;
use crate::video;

/// Published on the status track as JSON.
#[derive(Clone, serde::Serialize)]
pub struct State {
    /// Available action names.
    pub actions: Vec<String>,
    /// Connected viewer IDs.
    pub controllers: Vec<String>,
}

/// A command sent by a viewer.
#[derive(serde::Deserialize, Debug)]
#[serde(tag = "type")]
enum Command {
    #[serde(rename = "action")]
    Action { name: String },
    #[serde(rename = "kill")]
    Kill,
}

struct Inner {
    state: Mutex<State>,
    /// Sends commands directly to the game loop.
    cmd_tx: tokio::sync::mpsc::Sender<String>,
    action_names: Vec<String>,
}

#[derive(Clone)]
pub struct Drone {
    broadcast: moq_lite::BroadcastProducer,
    inner: Arc<Inner>,
}

impl Drone {
    pub fn new(_config: &Config, cmd_tx: tokio::sync::mpsc::Sender<String>) -> Self {
        let broadcast = moq_lite::BroadcastProducer::default();
        let action_names = game::action_names();

        Self {
            broadcast,
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    actions: action_names.clone(),
                    controllers: Vec::new(),
                }),
                cmd_tx,
                action_names,
            }),
        }
    }

    pub fn consume(&self) -> moq_lite::BroadcastConsumer {
        self.broadcast.consume()
    }

    pub async fn run(&self, cmd_rx: tokio::sync::mpsc::Receiver<String>) -> anyhow::Result<()> {
        let mut broadcast = self.broadcast.clone();

        // Catalog and video tracks are managed by the Avc3 importer.
        let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;

        // Create sensor track (raw JSON, not via catalog).
        let sensor_track = moq_lite::Track {
            name: "sensor".to_string(),
            priority: 10,
        };
        let sensor_producer = broadcast.create_track(sensor_track)?;

        // Create status track (raw JSON).
        let status_track = moq_lite::Track {
            name: "status".to_string(),
            priority: 10,
        };
        let status_producer = broadcast.create_track(status_track)?;

        // Shared battery level between game and sensor.
        let battery = sensor::new_battery();

        // Start the video/game pipeline.
        let video_handle = tokio::spawn({
            let broadcast = broadcast.clone();
            let catalog = catalog.clone();
            let battery = battery.clone();
            async move { video::run_pipeline(broadcast, catalog, cmd_rx, battery).await }
        });

        let sensor_handle = tokio::spawn(sensor::run_sensor(sensor_producer, battery));
        let state = self.inner.clone();
        let status_handle = tokio::spawn(run_status(status_producer, state));

        tokio::select! {
            res = video_handle => res?.context("video pipeline error"),
            res = sensor_handle => res?.context("sensor error"),
            res = status_handle => res?.context("status error"),
        }
    }
}

/// Publishes state changes to the status track.
async fn run_status(
    mut producer: moq_lite::TrackProducer,
    inner: Arc<Inner>,
) -> anyhow::Result<()> {
    let mut last_json = String::new();

    loop {
        let json = {
            let state = inner.state.lock().unwrap();
            serde_json::to_string(&*state)?
        };

        if json != last_json {
            let mut group = producer.append_group()?;
            group.write_frame(json.as_bytes().to_vec())?;
            group.finish()?;
            last_json = json;
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Handles discovered viewers: subscribes to their command tracks.
pub async fn handle_viewers(
    viewer_origin: &mut moq_lite::OriginConsumer,
    drone: &Drone,
) -> anyhow::Result<()> {
    loop {
        let Some((path, broadcast)) = viewer_origin.announced().await else {
            break;
        };

        let viewer_id = path.to_string();

        if let Some(broadcast) = broadcast {
            tracing::info!(%viewer_id, "viewer connected");
            drone
                .inner
                .state
                .lock()
                .unwrap()
                .controllers
                .push(viewer_id.clone());

            let inner = drone.inner.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_viewer_commands(&viewer_id, broadcast, &inner).await {
                    tracing::warn!(%viewer_id, error = %e, "viewer command error");
                }
                inner
                    .state
                    .lock()
                    .unwrap()
                    .controllers
                    .retain(|c| c != &viewer_id);
                tracing::info!(%viewer_id, "viewer disconnected");
            });
        } else {
            tracing::info!(%viewer_id, "viewer went offline");
            drone
                .inner
                .state
                .lock()
                .unwrap()
                .controllers
                .retain(|c| c != &viewer_id);
        }
    }
    Ok(())
}

async fn handle_viewer_commands(
    viewer_id: &str,
    broadcast: moq_lite::BroadcastConsumer,
    inner: &Arc<Inner>,
) -> anyhow::Result<()> {
    let command_track = moq_lite::Track {
        name: "command".to_string(),
        priority: 0,
    };

    let mut track = broadcast.subscribe_track(&command_track)?;

    while let Some(mut group) = track.next_group().await? {
        while let Some(frame) = group.read_frame().await? {
            let text = std::str::from_utf8(&frame)?;
            match serde_json::from_str::<Command>(text) {
                Ok(Command::Action { name }) => {
                    if !inner.action_names.contains(&name) {
                        tracing::warn!(%viewer_id, %name, "unknown action");
                        continue;
                    }
                    let _ = inner.cmd_tx.send(name.clone()).await;
                    tracing::info!(%viewer_id, %name, "action");
                }
                Ok(Command::Kill) => {
                    let _ = inner.cmd_tx.send("dock".to_string()).await;
                    tracing::info!(%viewer_id, "kill → dock");
                }
                Err(e) => {
                    tracing::warn!(%viewer_id, error = %e, "invalid command");
                }
            }
        }
    }

    Ok(())
}

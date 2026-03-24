use std::sync::{Arc, Mutex};

use anyhow::Context;

use crate::Config;
use crate::sensor;
use crate::video;

/// Shared robo state, updated by commands and read by the status publisher.
#[derive(Clone, serde::Serialize)]
pub struct State {
    pub angle: usize,
    pub controllers: Vec<String>,
    pub killed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            angle: 1,
            controllers: Vec::new(),
            killed: false,
        }
    }
}

/// A command sent by a viewer.
#[derive(serde::Deserialize, Debug)]
#[serde(tag = "type")]
enum Command {
    #[serde(rename = "angle")]
    Angle { value: usize },
    #[serde(rename = "kill")]
    Kill,
}

struct Inner {
    state: Mutex<State>,
    angle_switch: tokio::sync::watch::Sender<usize>,
    angles_len: usize,
}

#[derive(Clone)]
pub struct Robo {
    broadcast: moq_lite::BroadcastProducer,
    inner: Arc<Inner>,
    config: Config,
}

impl Robo {
    pub fn new(config: &Config) -> Self {
        let broadcast = moq_lite::BroadcastProducer::default();
        let (angle_tx, _) = tokio::sync::watch::channel(1usize);

        Self {
            broadcast,
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                angle_switch: angle_tx,
                angles_len: config.angles.len(),
            }),
            config: config.clone(),
        }
    }

    pub fn consume(&self) -> moq_lite::BroadcastConsumer {
        self.broadcast.consume()
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let mut broadcast = self.broadcast.clone();

        // Catalog and video tracks are managed by the Avc1/Avc3 importers.
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

        // Start the video pipeline (Avc1 for HD transmux, Avc3 for 240p transcode).
        let angle_rx = self.inner.angle_switch.subscribe();
        let video_handle = tokio::task::spawn_blocking({
            let angles = self.config.angles.clone();
            let broadcast = broadcast.clone();
            let catalog = catalog.clone();
            move || video::run_pipeline(angles, broadcast, catalog, angle_rx)
        });

        let sensor_handle = tokio::spawn(sensor::run_sensor(sensor_producer));
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
    robo: &Robo,
) -> anyhow::Result<()> {
    loop {
        let Some((path, broadcast)) = viewer_origin.announced().await else {
            break;
        };

        let viewer_id = path.to_string();

        if let Some(broadcast) = broadcast {
            tracing::info!(%viewer_id, "viewer connected");
            robo.inner
                .state
                .lock()
                .unwrap()
                .controllers
                .push(viewer_id.clone());

            let inner = robo.inner.clone();
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
            robo.inner
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
                Ok(Command::Angle { value }) => {
                    if value >= 1 && value <= inner.angles_len {
                        inner.state.lock().unwrap().angle = value;
                        let _ = inner.angle_switch.send(value);
                        tracing::info!(%viewer_id, angle = value, "switched angle");
                    }
                }
                Ok(Command::Kill) => {
                    inner.state.lock().unwrap().killed = true;
                    tracing::warn!(%viewer_id, "kill switch activated");
                }
                Err(e) => {
                    tracing::warn!(%viewer_id, error = %e, "invalid command");
                }
            }
        }
    }

    Ok(())
}

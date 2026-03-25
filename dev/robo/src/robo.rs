use std::sync::{Arc, Mutex};

use anyhow::Context;

use crate::sensor;
use crate::video;
use crate::{Config, MediaFiles};

/// Published on the status track as JSON.
#[derive(Clone, serde::Serialize)]
pub struct State {
    /// Available action names (sorted).
    pub actions: Vec<String>,
    /// Current state: "idle", "dead", or an action name.
    pub current: String,
    /// Queued action (replaces previous queue). None means return to idle after current action.
    pub queued: Option<String>,
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

/// Commands sent to the video pipeline.
#[derive(Clone, Debug)]
pub enum VideoCommand {
    /// Switch to an action file (plays once).
    Action(String),
    /// Kill switch — switch to dead loop immediately.
    Kill,
}

struct Inner {
    state: Mutex<State>,
    /// Sends commands to the video pipeline.
    cmd_tx: tokio::sync::watch::Sender<Option<VideoCommand>>,
    action_names: Vec<String>,
}

#[derive(Clone)]
pub struct Robo {
    broadcast: moq_lite::BroadcastProducer,
    inner: Arc<Inner>,
    media: MediaFiles,
}

impl Robo {
    pub fn new(_config: &Config, media: &MediaFiles) -> Self {
        let broadcast = moq_lite::BroadcastProducer::default();
        let (cmd_tx, _) = tokio::sync::watch::channel(None);

        let action_names: Vec<String> = media.actions.keys().cloned().collect();

        Self {
            broadcast,
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    actions: action_names.clone(),
                    current: "idle".to_string(),
                    queued: None,
                    controllers: Vec::new(),
                }),
                cmd_tx,
                action_names,
            }),
            media: media.clone(),
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

        // Channel for the video pipeline to signal action completion.
        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);

        // Start the video pipeline (async, uses block_in_place for ffmpeg calls).
        let cmd_rx = self.inner.cmd_tx.subscribe();
        let video_handle = tokio::spawn({
            let media = self.media.clone();
            let broadcast = broadcast.clone();
            let catalog = catalog.clone();
            async move { video::run_pipeline(media, broadcast, catalog, cmd_rx, done_tx).await }
        });

        // Handle action completions (video finished playing an action file).
        let inner = self.inner.clone();
        let done_handle = tokio::spawn(async move {
            while done_rx.recv().await.is_some() {
                let mut state = inner.state.lock().unwrap();
                if let Some(queued) = state.queued.take() {
                    // Play the queued action next.
                    state.current = queued.clone();
                    let _ = inner.cmd_tx.send(Some(VideoCommand::Action(queued)));
                } else {
                    // Return to idle.
                    state.current = "idle".to_string();
                    let _ = inner.cmd_tx.send(None);
                }
            }
            Ok::<_, anyhow::Error>(())
        });

        let sensor_handle = tokio::spawn(sensor::run_sensor(sensor_producer));
        let state = self.inner.clone();
        let status_handle = tokio::spawn(run_status(status_producer, state));

        tokio::select! {
            res = video_handle => res?.context("video pipeline error"),
            res = done_handle => res?.context("done handler error"),
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
                Ok(Command::Action { name }) => {
                    if !inner.action_names.contains(&name) {
                        tracing::warn!(%viewer_id, %name, "unknown action");
                        continue;
                    }

                    let mut state = inner.state.lock().unwrap();
                    if state.current == "idle" || state.current == "dead" {
                        // Currently looping — interrupt immediately.
                        state.current = name.clone();
                        state.queued = None;
                        let _ = inner.cmd_tx.send(Some(VideoCommand::Action(name)));
                        tracing::info!(%viewer_id, current = %state.current, "action started");
                    } else {
                        // Currently playing an action — queue this one.
                        state.queued = Some(name.clone());
                        tracing::info!(%viewer_id, queued = %name, "action queued");
                    }
                }
                Ok(Command::Kill) => {
                    let mut state = inner.state.lock().unwrap();
                    if state.current != "dead" {
                        state.current = "dead".to_string();
                        state.queued = None;
                        let _ = inner.cmd_tx.send(Some(VideoCommand::Kill));
                        tracing::warn!(%viewer_id, "kill switch activated");
                    }
                }
                Err(e) => {
                    tracing::warn!(%viewer_id, error = %e, "invalid command");
                }
            }
        }
    }

    Ok(())
}

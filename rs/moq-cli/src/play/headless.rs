//! Audio-only playback through the real speaker, without a display event loop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use hang::moq_net::origin;
use tokio::sync::{Notify, mpsc};

use moq_audio::playback::Observer;

use super::args::Args;
use super::media::Media;
use super::metrics::Metrics;
use super::output::Output;
use super::timeline::Presentation;
use super::window::Event;

/// The speaker and the run loop's events, plus `--metrics`' observer feed.
#[derive(Clone)]
struct Headless {
	events: mpsc::UnboundedSender<Event>,
	observe: Option<mpsc::UnboundedSender<Observer>>,
}

impl Output for Headless {
	type Speaker = moq_audio::playback::Engine;

	async fn speaker(&self) -> anyhow::Result<Self::Speaker> {
		let engine = moq_audio::playback::Engine::open(Default::default()).await?;
		if let Some(observe) = &self.observe {
			let _ = observe.send(engine.observe()?);
		}
		Ok(engine)
	}

	fn send(&self, event: Event) {
		let _ = self.events.send(event);
	}

	/// Never asked: headless playback decodes no video.
	async fn render(&self) -> moq_video::Output {
		moq_video::Output::Cpu
	}
}

pub(crate) async fn run(
	origin: origin::Consumer,
	broadcast: String,
	args: Args,
	mut network: tokio::task::JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<()> {
	let (events, mut received) = mpsc::unbounded_channel();
	let presentation = Arc::new(Mutex::new(Presentation::new(args.video_delay())));
	let (mut metrics, observe) = match &args.metrics {
		Some(path) => {
			let (metrics, observe) = Metrics::create(path, presentation.clone())?;
			(Some(metrics), Some(observe))
		}
		None => (None, None),
	};
	let output = Headless { events, observe };
	let mut media = tokio::spawn(
		Media {
			origin,
			broadcast,
			presentation,
			args,
			video: Arc::new(Mutex::new(VecDeque::new())),
			drained: Arc::new(Notify::new()),
			output,
		}
		.run(),
	);

	// One listener for the whole run: a fresh `ctrl_c()` per iteration misses a signal sent between polls.
	let ctrl_c = tokio::signal::ctrl_c();
	tokio::pin!(ctrl_c);
	let mut completed = false;
	let result = loop {
		tokio::select! {
			event = received.recv() => match event {
				Some(Event::Wake) => {}
				Some(Event::Ended | Event::Finished) => break Ok(()),
				Some(Event::Failed(error)) => break Err(anyhow::anyhow!(error)),
				None => break Err(anyhow::anyhow!("audio playback event channel closed")),
			},
			result = &mut media, if !completed => {
				completed = true;
				if let Err(error) = result {
					break Err(error).context("audio playback task failed");
				}
				// Read the queued final event before reporting successful task completion.
			},
			result = network.join_next(), if !network.is_empty() => {
				break match result {
					Some(Ok(result)) => result.context("MoQ transport failed"),
					Some(Err(error)) => Err(error).context("MoQ transport task failed"),
					None => Ok(()),
				};
			},
			result = &mut ctrl_c => break result.context("failed to wait for Ctrl-C"),
			result = async { metrics.as_mut().expect("guarded").tick().await }, if metrics.is_some() => {
				if let Err(error) = result {
					break Err(error);
				}
			}
		}
	};
	// While the speaker still plays, so the last sample closes any gap it is in.
	let result = match metrics {
		Some(metrics) if result.is_ok() => metrics.finish().await,
		_ => result,
	};
	media.abort();
	// A completed handle must not be polled twice.
	if !completed {
		let _ = media.await;
	}
	network.shutdown().await;
	result
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn audio_only_needs_no_display_event_loop() {
		let producer = moq_tokio::origin::spawn();
		let args = Args {
			catalog_format: None,
			metrics: None,
			delay: super::super::args::Delay::Auto,
			select: crate::subscribe::SelectArgs {
				video: Some(crate::subscribe::Mode::Off),
				..Default::default()
			},
		};
		let mut network = tokio::task::JoinSet::new();
		network.spawn(async { Ok(()) });
		super::super::run(producer.consume(), "audio".into(), args, network)
			.await
			.unwrap();
	}

	/// A run that ends still writes its final sample, with nothing it could not
	/// measure filled in.
	#[tokio::test]
	async fn metrics_write_a_final_sample() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("play.ndjson");
		let producer = moq_tokio::origin::spawn();
		let args = Args {
			catalog_format: None,
			metrics: Some(path.clone()),
			delay: super::super::args::Delay::Auto,
			select: crate::subscribe::SelectArgs {
				video: Some(crate::subscribe::Mode::Off),
				..Default::default()
			},
		};
		let mut network = tokio::task::JoinSet::new();
		network.spawn(async { Ok(()) });
		super::super::run(producer.consume(), "audio".into(), args, network)
			.await
			.unwrap();

		let written = std::fs::read_to_string(&path).unwrap();
		let last: serde_json::Value = serde_json::from_str(written.lines().last().unwrap()).unwrap();
		assert!(last["at"].is_f64(), "{last}");
		for unmeasured in ["render", "timestamp", "quanta", "gaps"] {
			assert!(last.get(unmeasured).is_none(), "{unmeasured} with no speaker: {last}");
		}
	}

	#[tokio::test]
	async fn transport_failure_reaches_the_headless_caller() {
		let producer = moq_tokio::origin::spawn();
		let args = Args {
			catalog_format: None,
			metrics: None,
			delay: super::super::args::Delay::Auto,
			select: crate::subscribe::SelectArgs {
				video: Some(crate::subscribe::Mode::Off),
				..Default::default()
			},
		};
		let mut network = tokio::task::JoinSet::new();
		network.spawn(async { anyhow::bail!("transport test failure") });
		let error = super::super::run(producer.consume(), "audio".into(), args, network)
			.await
			.unwrap_err();
		assert!(format!("{error:#}").contains("transport test failure"));
	}
}

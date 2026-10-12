//! Audio-only playback through the real speaker, without a display event loop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use hang::moq_net::origin;
use tokio::sync::{Notify, mpsc};

use super::args::Args;
use super::media::Media;
use super::output::Output;
use super::timeline::Presentation;
use super::window::Event;

impl Output for mpsc::UnboundedSender<Event> {
	type Speaker = moq_audio::playback::Engine;

	async fn speaker(&self) -> anyhow::Result<Self::Speaker> {
		Ok(moq_audio::playback::Engine::open(Default::default()).await?)
	}

	fn send(&self, event: Event) {
		let _ = self.send(event);
	}
}

pub(crate) async fn run(
	origin: origin::Consumer,
	broadcast: String,
	args: Args,
	mut network: tokio::task::JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<()> {
	let (output, mut events) = mpsc::unbounded_channel();
	let mut media = tokio::spawn(
		Media {
			origin,
			broadcast,
			presentation: Arc::new(Mutex::new(Presentation::new(args.video_delay()))),
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
			event = events.recv() => match event {
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
		}
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
			delay: super::super::args::Delay::Auto,
			select: crate::subscribe::SelectArgs {
				no_video: true,
				..Default::default()
			},
		};
		let mut network = tokio::task::JoinSet::new();
		network.spawn(async { Ok(()) });
		super::super::run(producer.consume(), "audio".into(), args, network)
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn transport_failure_reaches_the_headless_caller() {
		let producer = moq_tokio::origin::spawn();
		let args = Args {
			catalog_format: None,
			delay: super::super::args::Delay::Auto,
			select: crate::subscribe::SelectArgs {
				no_video: true,
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

//! `moq play --metrics`: what the speaker played, sampled every 250ms as NDJSON.
//!
//! Each line is one `Sample` of the audio-quality lane's metric schema
//! (`test/audio-quality/clients/js/src/schema.ts`), with the same field names
//! and meanings, so its analyzer reduces a native run exactly as it does a
//! browser one. Durations are milliseconds as floats. `at` is this process's
//! monotonic clock from the first sample, `render` the device's clock, and
//! `timestamp` the media clock; fields this player cannot read are left out
//! rather than written as zero.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use kio::clock::Instant;
use moq_audio::playback::{Observation, Observer};
use serde::Serialize;
use tokio::sync::mpsc;

use super::timeline::Presentation;

/// How often a sample is taken, the browser probe's interval.
const INTERVAL: Duration = Duration::from_millis(250);

/// How long to wait for the speaker to close out the measurement: four samples.
const FINISH_WAIT: Duration = INTERVAL.saturating_mul(4);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
	at: f64,
	#[serde(skip_serializing_if = "Option::is_none")]
	render: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	timestamp: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	stalled: Option<bool>,
	#[serde(skip_serializing_if = "Option::is_none")]
	delay: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	output_latency: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	quanta: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	quiet: Option<u64>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	gaps: Vec<Gap>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	stalls: Vec<Stall>,
}

#[derive(Serialize)]
struct Gap {
	at: f64,
	ms: f64,
	quanta: u64,
	short: u64,
}

#[derive(Serialize)]
struct Stall {
	at: f64,
	stalled: bool,
}

fn ms(duration: Duration) -> f64 {
	duration.as_secs_f64() * 1000.0
}

/// Running totals carried over from engines that have closed, so a speaker
/// reopened after an idle spell continues the counts instead of restarting them.
#[derive(Clone, Copy, Default)]
struct Totals {
	clock: Duration,
	quanta: u64,
	quiet: u64,
}

impl Totals {
	fn of(observation: &Observation) -> Self {
		Self {
			clock: observation.clock,
			quanta: observation.quanta,
			quiet: observation.quiet,
		}
	}
}

/// The writer, fed an [`Observer`] each time playback opens a speaker.
pub(super) struct Metrics {
	file: BufWriter<File>,
	presentation: Arc<Mutex<Presentation>>,
	observers: mpsc::UnboundedReceiver<Observer>,
	current: Option<(Observer, Totals)>,
	/// What closed engines counted, added to the current one's totals.
	base: Totals,
	start: Instant,
	interval: kio::clock::Interval,
}

impl Metrics {
	/// Start writing to `path`, returning where to send each new speaker's observer.
	pub(super) fn create(
		path: &Path,
		presentation: Arc<Mutex<Presentation>>,
	) -> anyhow::Result<(Self, mpsc::UnboundedSender<Observer>)> {
		let file = File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
		let (observe, observers) = mpsc::unbounded_channel();
		let mut interval = kio::clock::interval(INTERVAL);
		interval.set_missed_tick_behavior(kio::clock::MissedTickBehavior::Delay);
		let metrics = Self {
			file: BufWriter::new(file),
			presentation,
			observers,
			current: None,
			base: Totals::default(),
			start: Instant::now(),
			interval,
		};
		Ok((metrics, observe))
	}

	/// Wait for the next sample and write it. Cancel safe.
	pub(super) async fn tick(&mut self) -> anyhow::Result<()> {
		self.interval.tick().await;
		self.sample()
	}

	/// End the measurement: close any gap still open and write one last sample.
	pub(super) async fn finish(mut self) -> anyhow::Result<()> {
		// A speaker reopened since the last sample is the one playing now: adopt it
		// first, so it is the one told to finish.
		if !self.observers.is_empty() {
			self.sample()?;
		}
		if let Some((observer, _)) = &mut self.current {
			// The audio thread closes the gap on its next buffer. One that never
			// comes is a device that stopped pulling, which the render clock
			// already shows, so the wait is bounded rather than hung on it.
			if kio::clock::timeout(FINISH_WAIT, observer.finish()).await.is_err() {
				tracing::warn!("the speaker never closed out the measurement; the last gap may be missing");
			}
		}
		self.tick().await
	}

	fn sample(&mut self) -> anyhow::Result<()> {
		let mut gaps = Vec::new();
		let mut stalls = Vec::new();

		// A closed engine's observer still holds whatever it counted after the last
		// read, so drain it before moving its totals into the base.
		while let Ok(next) = self.observers.try_recv() {
			if let Some((mut observer, base)) = self.current.take() {
				let last = observer.read().context("speaker metrics")?;
				collect(&last, base, &mut gaps, &mut stalls);
				let totals = Totals::of(&last);
				self.base = Totals {
					clock: base.clock + totals.clock,
					quanta: base.quanta + totals.quanta,
					quiet: base.quiet + totals.quiet,
				};
			}
			self.current = Some((next, self.base));
		}

		let observation = match &mut self.current {
			Some((observer, base)) => {
				let observation = observer.read().context("speaker metrics")?;
				collect(&observation, *base, &mut gaps, &mut stalls);
				Some((observation, *base))
			}
			None => None,
		};
		// Read straight after the device clock, so the two pair up.
		let now = Instant::now();
		let (playhead, target) = {
			let presentation = self.presentation.lock().unwrap();
			(presentation.playhead(now), presentation.target())
		};

		let sample = Sample {
			at: ms(now.duration_since(self.start)),
			render: observation.as_ref().map(|(o, base)| ms(base.clock + o.clock)),
			timestamp: playhead.map(ms),
			stalled: observation.as_ref().map(|(o, _)| o.stalled),
			delay: target.map(ms),
			output_latency: observation.as_ref().and_then(|(o, _)| o.latency).map(ms),
			quanta: observation.as_ref().map(|(o, base)| base.quanta + o.quanta),
			quiet: observation.as_ref().map(|(o, base)| base.quiet + o.quiet),
			gaps,
			stalls,
		};
		serde_json::to_writer(&mut self.file, &sample)?;
		self.file.write_all(b"\n")?;
		// Flushed per line, so a run that dies still leaves everything before it.
		self.file.flush()?;
		Ok(())
	}
}

/// Move an observation's events onto the shared clock.
fn collect(observation: &Observation, base: Totals, gaps: &mut Vec<Gap>, stalls: &mut Vec<Stall>) {
	gaps.extend(observation.gaps.iter().map(|gap| Gap {
		at: ms(base.clock + gap.at),
		ms: ms(gap.missing),
		quanta: gap.quanta,
		short: gap.short,
	}));
	stalls.extend(observation.stalls.iter().map(|stall| Stall {
		at: ms(base.clock + stall.at),
		stalled: stall.stalled,
	}));
}

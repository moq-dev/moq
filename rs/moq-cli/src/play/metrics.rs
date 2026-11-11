//! `moq play --metrics`: what the speaker played, sampled every 250ms as NDJSON.
//!
//! Each line is one `Sample` of the audio-quality lane's metric schema
//! (`test/audio-quality/clients/js/src/schema.ts`), with the same field names
//! and meanings, so its analyzer reduces a native run exactly as it does a
//! browser one. Durations are milliseconds as floats. `at` is this process's
//! monotonic clock from the first sample, `render` the device's clock, and
//! `timestamp` the media clock, with `clock` where the presentation clock video
//! plays from stands on it at the same instant; fields this player cannot read are left out
//! rather than written as zero. `comfort` and `utterances` have no browser
//! counterpart: they show how a declared silence played, for push-to-talk.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use kio::clock::Instant;
use moq_audio::playback::{Observation, Observer};
use serde::Serialize;
use tokio::sync::mpsc;

use moq_mux::sync::Consumer;

use super::timeline::Playhead;

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
	clock: Option<f64>,
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
	#[serde(skip_serializing_if = "Option::is_none")]
	comfort: Option<f64>,
	player_skips: u64,
	trimmed: f64,
	#[serde(skip_serializing_if = "Option::is_none")]
	accelerated: Option<f64>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	gaps: Vec<Gap>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	stalls: Vec<Stall>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	utterances: Vec<Utterance>,
}

/// Audio resuming after a declared silence: when it was written, and the media
/// timestamp of its first sample.
#[derive(Serialize)]
struct Utterance {
	at: f64,
	timestamp: f64,
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
	comfort: Duration,
	skips: u64,
	trimmed: Duration,
	accelerated: Duration,
}

impl Totals {
	fn of(observation: &Observation) -> Self {
		Self {
			clock: observation.clock,
			quanta: observation.quanta,
			quiet: observation.quiet,
			comfort: observation.comfort,
			skips: observation.skips,
			trimmed: observation.trimmed,
			accelerated: observation.accelerated,
		}
	}

	fn add(self, other: Self) -> Self {
		Self {
			clock: self.clock + other.clock,
			quanta: self.quanta + other.quanta,
			quiet: self.quiet + other.quiet,
			comfort: self.comfort + other.comfort,
			skips: self.skips + other.skips,
			trimmed: self.trimmed + other.trimmed,
			accelerated: self.accelerated + other.accelerated,
		}
	}
}

/// The writer, fed an [`Observer`] each time playback opens a speaker.
pub(super) struct Metrics {
	file: BufWriter<File>,
	presentation: Consumer,
	playhead: Playhead,
	observers: mpsc::UnboundedReceiver<Observer>,
	current: Option<(Observer, Totals)>,
	/// What closed engines counted, added to the current one's totals.
	base: Totals,
	/// Utterances since the last sample.
	utterances: Vec<Utterance>,
	/// Received audio the player discarded before it reached a speaker's queue,
	/// and the skips that did it.
	trimmed: Duration,
	skips: u64,
	start: Instant,
	interval: kio::clock::Interval,
}

impl Metrics {
	/// Start writing to `path`, returning where to send each new speaker's observer.
	pub(super) fn create(
		path: &Path,
		presentation: Consumer,
		playhead: Playhead,
	) -> anyhow::Result<(Self, mpsc::UnboundedSender<Observer>)> {
		let file = File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
		let (observe, observers) = mpsc::unbounded_channel();
		let mut interval = kio::clock::interval(INTERVAL);
		interval.set_missed_tick_behavior(kio::clock::MissedTickBehavior::Delay);
		let metrics = Self {
			file: BufWriter::new(file),
			presentation,
			playhead,
			observers,
			current: None,
			base: Totals::default(),
			utterances: Vec::new(),
			trimmed: Duration::ZERO,
			skips: 0,
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

	/// Note audio resuming after a declared silence at media `timestamp`.
	pub(super) fn utterance(&mut self, timestamp: Duration) {
		self.utterances.push(Utterance {
			at: ms(Instant::now().duration_since(self.start)),
			timestamp: ms(timestamp),
		});
	}

	/// Count received audio the player discarded rather than handed to the
	/// speaker, and whether that started a skip.
	pub(super) fn trimmed(&mut self, audio: Duration, skip: bool) {
		self.trimmed += audio;
		self.skips += skip as u64;
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
				self.base = base.add(Totals::of(&last));
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
		let totals = observation
			.as_ref()
			.map_or(self.base, |(o, base)| base.add(Totals::of(o)));
		// Read straight after the device clock, so the two pair up.
		let now = Instant::now();
		let (playhead, delay) = (self.playhead.at(now), self.presentation.delay());
		let clock = self.presentation.position(now);

		let sample = Sample {
			at: ms(now.duration_since(self.start)),
			render: observation.as_ref().map(|_| ms(totals.clock)),
			timestamp: playhead.map(ms),
			clock: clock.map(ms),
			stalled: observation.as_ref().map(|(o, _)| o.stalled),
			delay: Some(ms(delay)),
			output_latency: observation.as_ref().and_then(|(o, _)| o.latency).map(ms),
			quanta: observation.as_ref().map(|_| totals.quanta),
			quiet: observation.as_ref().map(|_| totals.quiet),
			comfort: observation.as_ref().map(|_| ms(totals.comfort)),
			player_skips: self.skips + totals.skips,
			trimmed: ms(self.trimmed + totals.trimmed),
			accelerated: observation.as_ref().map(|_| ms(totals.accelerated)),
			gaps,
			stalls,
			utterances: std::mem::take(&mut self.utterances),
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

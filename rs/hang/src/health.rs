//! Client health from two consecutive media reports.
//!
//! `publisher` and `viewer` turn a `Sample` of a publisher's `stats` snapshot or a viewer's
//! `echo` feedback into counter deltas over the stated interval, graded against `Thresholds`.
//! Container sections such as `mpegts` are never read. A client with no report has no sample:
//! its verdict is `Verdict::Unknown`, the default.
use crate::snapshot::Millis;
use crate::{echo, stats};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

/// A grade, ordered by severity with [Verdict::Unknown] below every graded one.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
	/// Nothing could be graded, or the report is stale.
	#[default]
	Unknown,
	/// Every graded input is within its limits.
	Healthy,
	/// A graded input reached its degraded limit.
	Degraded,
	/// A graded input reached its unhealthy limit.
	Unhealthy,
}

/// Which client wrote a report, and so which of its numbers can be graded.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Role {
	/// A publisher's `stats` self-report.
	Publisher,
	/// A viewer's `.echo` feedback.
	Viewer,
}

/// The broadcast a report was read from, which observed every number in it.
#[serde_with::serde_as]
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Observer {
	/// The publisher's media broadcast, or the viewer's echo broadcast.
	pub path: moq_net::PathOwned,
	/// The reporting broadcast's publisher epoch, when its route carries one.
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub epoch: Option<moq_net::Epoch>,
}

impl Observer {
	/// Name the broadcast a report was read from.
	pub fn new(path: moq_net::PathOwned, epoch: Option<moq_net::Epoch>) -> Self {
		Self { path, epoch }
	}
}

/// One snapshot and the broadcast it was read from.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Report<S> {
	/// Who observed the snapshot's numbers.
	pub observer: Observer,
	/// The cumulative snapshot.
	pub snapshot: S,
}

impl<S> Report<S> {
	/// Pair a snapshot with the broadcast it was read from.
	pub fn new(observer: Observer, snapshot: S) -> Self {
		Self { observer, snapshot }
	}
}

/// Two consecutive reports from one client and the time between them.
#[derive(Debug)]
#[non_exhaustive]
pub struct Sample<'a, S> {
	/// The report before `current`, or `None` for the first, which has nothing to diff against.
	pub previous: Option<&'a Report<S>>,
	/// The newest report.
	pub current: &'a Report<S>,
	/// The time between the two reports, over which counter deltas become rates.
	pub interval: Duration,
	/// How long ago `current` was reported. Past [Thresholds::stale] every verdict is unknown.
	pub age: Duration,
}

impl<'a, S> Sample<'a, S> {
	/// Grade `current`, reported `age` ago, with nothing to diff it against.
	pub fn new(current: &'a Report<S>, age: Duration) -> Self {
		Self {
			previous: None,
			current,
			interval: Duration::ZERO,
			age,
		}
	}

	/// Diff against `previous`, reported `interval` before `current`.
	pub fn with_previous(mut self, previous: &'a Report<S>, interval: Duration) -> Self {
		self.previous = Some(previous);
		self.interval = interval;
		self
	}
}

/// The values at which an input grades degraded and unhealthy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits<T> {
	/// Degraded from this value on (below it, for [TransportThresholds::headroom]).
	pub degraded: T,
	/// Unhealthy from this value on (below it, for [TransportThresholds::headroom]).
	pub unhealthy: T,
}

/// Grading limits, nested by the report section they apply to.
///
/// [Default] holds the documented defaults; override a field to change one. The transport limits
/// come from a paced 9.2 Mb/s TS feed through a shaped link, graded over five-second intervals:
/// unimpaired and 80 ms round-trip links kept RTT at most 107 ms and headroom above 4.6; a 12 Mb/s
/// link kept headroom at 1.67 or more and lost at most 0.23% of media bytes with delivery intact;
/// a 10 Mb/s link fell to 1.35. The RTT limits double ITU-T G.114's one-way delays for interactive
/// use. No CLI reporter emits the rendition counters, so their shares are not measured; a 1% rise
/// in time spent buffering cost viewers of a 90-minute live event over three minutes of watching
/// (Dobrian et al., SIGCOMM 2011).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Thresholds {
	/// A report older than this grades unknown, as does a transport sample whose age plus the report's exceeds it.
	pub stale: Duration,
	/// Limits for the reporting connection.
	pub transport: TransportThresholds,
	/// Limits for a publisher's renditions.
	pub publisher: PublisherThresholds,
	/// Limits for a viewer's renditions.
	pub viewer: ViewerThresholds,
}

/// Limits for the reporting connection.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TransportThresholds {
	/// Round-trip time.
	pub rtt: Limits<Duration>,
	/// Transport bytes lost per media byte sent (publisher only).
	pub loss: Limits<f64>,
	/// Estimated rate over the media bitrate sent (publisher only); lower is worse.
	pub headroom: Limits<f64>,
}

/// Limits for a publisher's renditions.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PublisherThresholds {
	/// Frames skipped before sending, as a share of frames sent or skipped.
	pub skipped: Limits<f64>,
}

/// Limits for a viewer's renditions.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ViewerThresholds {
	/// Frames received too late for playout, per frame received.
	pub late: Limits<f64>,
	/// Decode errors per frame received.
	pub errors: Limits<f64>,
	/// The share of the interval spent stalled.
	pub stalled: Limits<f64>,
}

impl Default for Thresholds {
	fn default() -> Self {
		let share = Limits {
			degraded: 0.01,
			unhealthy: 0.05,
		};
		Self {
			stale: Duration::from_secs(5),
			transport: TransportThresholds {
				rtt: Limits {
					degraded: Duration::from_millis(300),
					unhealthy: Duration::from_millis(800),
				},
				loss: Limits {
					degraded: 0.02,
					unhealthy: 0.1,
				},
				headroom: Limits {
					degraded: 1.5,
					unhealthy: 1.0,
				},
			},
			publisher: PublisherThresholds { skipped: share },
			viewer: ViewerThresholds {
				late: share,
				errors: share,
				stalled: share,
			},
		}
	}
}

/// Why a sample's deltas started over instead of spanning the interval.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Reset {
	/// A cumulative counter went down.
	Decrease,
	/// The reporting broadcast's path or epoch changed.
	Replaced,
}

/// An input a verdict can rest on, named by its unit.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Input {
	/// Round-trip time, in milliseconds.
	Rtt,
	/// Transport bytes lost per media byte sent over the interval.
	Loss,
	/// Estimated rate over the media bitrate sent over the interval; lower is worse.
	Headroom,
	/// Frames skipped before sending, as a share of frames sent or skipped over the interval.
	Skipped,
	/// Frames received too late for playout, per frame received over the interval.
	Late,
	/// Decode errors per frame received over the interval.
	Errors,
	/// The share of the interval spent stalled.
	Stalled,
}

/// One input's value and the grade it earned.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Evidence {
	/// The input graded.
	pub input: Input,
	/// The value in the input's unit, absent when the reports cannot show it.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub value: Option<f64>,
	/// The value's grade: unknown when the value is absent or stale.
	pub verdict: Verdict,
}

/// The reporting connection's grade and its loss over the interval.
#[derive(Serialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Connection {
	/// The worst graded evidence, or unknown when none was graded.
	pub verdict: Verdict,
	/// Every input graded for the role, in a fixed order.
	pub evidence: Vec<Evidence>,
	/// The transport sample plus the report's age exceeds [Thresholds::stale], so its evidence grades unknown.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub stale: bool,
	/// Bytes lost over the interval.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub lost_bytes: Option<u64>,
	/// Packets lost over the interval.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub lost_packets: Option<u64>,
}

/// One rendition's grade and its delivery over the interval.
#[derive(Serialize, Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Rendition {
	/// The worst graded evidence, or unknown when none was graded.
	pub verdict: Verdict,
	/// Every input graded for the role, in a fixed order.
	pub evidence: Vec<Evidence>,
	/// Frames sent or received over the interval.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub frames: Option<u64>,
	/// Media payload bytes sent or received over the interval.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub bytes: Option<u64>,
	/// Media bitrate over the interval, in bits per second.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub bitrate: Option<u64>,
}

/// A client's health over one sample, every number observed by [Health::observer].
#[serde_with::serde_as]
#[derive(Serialize, Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Health {
	/// Which client wrote the reports.
	pub role: Role,
	/// The broadcast the current report was read from.
	pub observer: Observer,
	/// The worst verdict of the connection and every rendition.
	pub verdict: Verdict,
	/// The report is older than [Thresholds::stale], so every verdict is unknown.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub stale: bool,
	/// The interval the deltas span; absent without a previous report to diff against.
	#[serde_as(as = "Option<Millis>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub interval: Option<Duration>,
	/// Why the deltas started over instead of spanning the interval.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub reset: Option<Reset>,
	/// The reporting connection.
	pub transport: Connection,
	/// Each rendition in the current report, keyed by its catalog ID.
	pub renditions: BTreeMap<String, Rendition>,
}

/// Grade a publisher's self-report.
///
/// Loss and headroom divide the connection's transport counters by this broadcast's media, so
/// they hold when the broadcast is the only one its connection publishes.
pub fn publisher<E>(sample: &Sample<'_, stats::Snapshot<E>>, thresholds: &Thresholds) -> Health {
	assess(sample, thresholds)
}

/// Grade a viewer's feedback.
pub fn viewer<E>(sample: &Sample<'_, echo::Snapshot<E>>, thresholds: &Thresholds) -> Health {
	assess(sample, thresholds)
}

/// What differs between grading a publisher's report and a viewer's.
trait Kind {
	type Rendition: Default;
	const ROLE: Role;
	fn transport(&self) -> &stats::Transport;
	fn renditions(&self) -> &BTreeMap<String, Self::Rendition>;
	fn frames(rendition: &Self::Rendition) -> u64;
	fn bytes(rendition: &Self::Rendition) -> u64;
	fn decreased(previous: &Self::Rendition, current: &Self::Rendition) -> bool;
	fn evidence(delta: Option<Delta<'_, Self::Rendition>>, thresholds: &Thresholds) -> Vec<Evidence>;
}

/// One rendition's previous and current counters, and the interval between them.
struct Delta<'a, R> {
	previous: &'a R,
	current: &'a R,
	interval: Duration,
}

impl<R> Delta<'_, R> {
	fn count(&self, counter: impl Fn(&R) -> u64) -> u64 {
		counter(self.current).saturating_sub(counter(self.previous))
	}
}

impl<E> Kind for stats::Snapshot<E> {
	type Rendition = stats::Rendition;
	const ROLE: Role = Role::Publisher;

	fn transport(&self) -> &stats::Transport {
		&self.transport
	}
	fn renditions(&self) -> &BTreeMap<String, stats::Rendition> {
		&self.renditions
	}
	fn frames(rendition: &stats::Rendition) -> u64 {
		rendition.frames
	}
	fn bytes(rendition: &stats::Rendition) -> u64 {
		rendition.bytes
	}
	fn decreased(previous: &stats::Rendition, current: &stats::Rendition) -> bool {
		current.frames < previous.frames
			|| current.bytes < previous.bytes
			|| current.keyframes < previous.keyframes
			|| current.skipped < previous.skipped
	}
	fn evidence(delta: Option<Delta<'_, stats::Rendition>>, thresholds: &Thresholds) -> Vec<Evidence> {
		let skipped = delta.and_then(|d| {
			let skipped = d.count(|r| r.skipped);
			ratio(skipped, d.count(|r| r.frames).saturating_add(skipped))
		});
		vec![above(Input::Skipped, skipped, &thresholds.publisher.skipped)]
	}
}

impl<E> Kind for echo::Snapshot<E> {
	type Rendition = echo::Rendition;
	const ROLE: Role = Role::Viewer;

	fn transport(&self) -> &stats::Transport {
		&self.transport
	}
	fn renditions(&self) -> &BTreeMap<String, echo::Rendition> {
		&self.renditions
	}
	fn frames(rendition: &echo::Rendition) -> u64 {
		rendition.frames
	}
	fn bytes(rendition: &echo::Rendition) -> u64 {
		rendition.bytes
	}
	fn decreased(previous: &echo::Rendition, current: &echo::Rendition) -> bool {
		current.frames < previous.frames
			|| current.bytes < previous.bytes
			|| current.decoded < previous.decoded
			|| current.late < previous.late
			|| current.errors < previous.errors
			|| current.stalls < previous.stalls
			|| current.stalled < previous.stalled
			|| current.underruns < previous.underruns
	}
	fn evidence(delta: Option<Delta<'_, echo::Rendition>>, thresholds: &Thresholds) -> Vec<Evidence> {
		let limits = &thresholds.viewer;
		let per_frame = |counter: fn(&echo::Rendition) -> u64| {
			delta
				.as_ref()
				.and_then(|d| ratio(d.count(counter), d.count(|r| r.frames)))
		};
		let stalled = delta.as_ref().map(|d| {
			let stalled = d.current.stalled.saturating_sub(d.previous.stalled);
			stalled.as_nanos() as f64 / d.interval.as_nanos() as f64
		});
		vec![
			above(Input::Late, per_frame(|r| r.late), &limits.late),
			above(Input::Errors, per_frame(|r| r.errors), &limits.errors),
			above(Input::Stalled, stalled, &limits.stalled),
		]
	}
}

fn assess<S: Kind>(sample: &Sample<'_, S>, thresholds: &Thresholds) -> Health {
	let current = &sample.current.snapshot;
	let (previous, reset) = match sample.previous {
		None => (None, None),
		Some(previous) if previous.observer != sample.current.observer => (None, Some(Reset::Replaced)),
		Some(previous) if decreased(&previous.snapshot, current) => (None, Some(Reset::Decrease)),
		Some(_) if sample.interval.is_zero() => (None, None),
		Some(previous) => (Some(&previous.snapshot), None),
	};
	let interval = previous.map(|_| sample.interval);

	let zero = S::Rendition::default();
	let mut sent: u64 = 0;
	let mut renditions = BTreeMap::new();
	for (id, rendition) in current.renditions() {
		let delta = previous.map(|previous| Delta {
			previous: previous.renditions().get(id).unwrap_or(&zero),
			current: rendition,
			interval: sample.interval,
		});
		let bytes = delta.as_ref().map(|d| d.count(S::bytes));
		sent = sent.saturating_add(bytes.unwrap_or(0));
		let frames = delta.as_ref().map(|d| d.count(S::frames));
		let evidence = S::evidence(delta, thresholds);
		renditions.insert(
			id.clone(),
			Rendition {
				verdict: worst(&evidence),
				evidence,
				frames,
				bytes,
				bitrate: bytes.zip(interval).map(|(bytes, interval)| bitrate(bytes, interval)),
			},
		);
	}

	let link = Link {
		current: current.transport(),
		previous: previous.map(S::transport),
		age: sample.age,
		sent,
		media: interval.map(|interval| bitrate(sent, interval)),
	};
	let transport = connection(&link, S::ROLE, thresholds);
	let mut health = Health {
		role: S::ROLE,
		observer: sample.current.observer.clone(),
		verdict: Verdict::Unknown,
		stale: sample.age > thresholds.stale,
		interval,
		reset,
		transport,
		renditions,
	};
	if health.stale {
		health.transport.verdict = Verdict::Unknown;
		forget(&mut health.transport.evidence);
		for rendition in health.renditions.values_mut() {
			rendition.verdict = Verdict::Unknown;
			forget(&mut rendition.evidence);
		}
	}
	health.verdict = health
		.renditions
		.values()
		.map(|rendition| rendition.verdict)
		.fold(health.transport.verdict, Verdict::max);
	health
}

/// The reporting connection's two samples, the current report's age, and the media sent between them.
struct Link<'a> {
	current: &'a stats::Transport,
	previous: Option<&'a stats::Transport>,
	age: Duration,
	sent: u64,
	media: Option<u64>,
}

/// Grade the connection: RTT for both roles, and loss and headroom where the publisher's sent
/// media gives them a denominator. A viewer's loss counters describe its own uplink.
fn connection(link: &Link<'_>, role: Role, thresholds: &Thresholds) -> Connection {
	let limits = &thresholds.transport;
	let lost = |counter: fn(&stats::Transport) -> Option<u64>| {
		link.previous
			.and_then(|previous| Some(counter(link.current)?.saturating_sub(counter(previous)?)))
	};
	let lost_bytes = lost(|t| t.lost_bytes);
	let lost_packets = lost(|t| t.lost_packets);

	let rtt = link.current.rtt.map(|rtt| rtt.as_nanos() as f64 / 1e6);
	let mut evidence = vec![above(
		Input::Rtt,
		rtt,
		&Limits {
			degraded: limits.rtt.degraded.as_nanos() as f64 / 1e6,
			unhealthy: limits.rtt.unhealthy.as_nanos() as f64 / 1e6,
		},
	)];
	if role == Role::Publisher {
		let loss = lost_bytes.and_then(|lost| ratio(lost, link.sent));
		let headroom = link
			.current
			.rate
			.zip(link.media.filter(|media| *media > 0))
			.map(|(rate, media)| rate as f64 / media as f64);
		evidence.push(above(Input::Loss, loss, &limits.loss));
		evidence.push(below(Input::Headroom, headroom, &limits.headroom));
	}

	let stale = link
		.current
		.age
		.is_some_and(|age| age.saturating_add(link.age) > thresholds.stale);
	if stale {
		forget(&mut evidence);
	}
	Connection {
		verdict: worst(&evidence),
		evidence,
		stale,
		lost_bytes,
		lost_packets,
	}
}

fn decreased<S: Kind>(previous: &S, current: &S) -> bool {
	let lost = |counter: fn(&stats::Transport) -> Option<u64>| {
		counter(previous.transport())
			.zip(counter(current.transport()))
			.is_some_and(|(previous, current)| current < previous)
	};
	lost(|t| t.lost_bytes)
		|| lost(|t| t.lost_packets)
		|| current.renditions().iter().any(|(id, rendition)| {
			previous
				.renditions()
				.get(id)
				.is_some_and(|previous| S::decreased(previous, rendition))
		})
}

fn ratio(count: u64, of: u64) -> Option<f64> {
	(of > 0).then(|| count as f64 / of as f64)
}

fn bitrate(bytes: u64, interval: Duration) -> u64 {
	let bits = u128::from(bytes) * 8 * 1_000_000_000;
	u64::try_from(bits / interval.as_nanos()).unwrap_or(u64::MAX)
}

fn above(input: Input, value: Option<f64>, limits: &Limits<f64>) -> Evidence {
	let verdict = match value {
		None => Verdict::Unknown,
		Some(value) if value >= limits.unhealthy => Verdict::Unhealthy,
		Some(value) if value >= limits.degraded => Verdict::Degraded,
		Some(_) => Verdict::Healthy,
	};
	Evidence { input, value, verdict }
}

fn below(input: Input, value: Option<f64>, limits: &Limits<f64>) -> Evidence {
	let verdict = match value {
		None => Verdict::Unknown,
		Some(value) if value < limits.unhealthy => Verdict::Unhealthy,
		Some(value) if value < limits.degraded => Verdict::Degraded,
		Some(_) => Verdict::Healthy,
	};
	Evidence { input, value, verdict }
}

fn worst(evidence: &[Evidence]) -> Verdict {
	evidence.iter().map(|e| e.verdict).max().unwrap_or_default()
}

fn forget(evidence: &mut [Evidence]) {
	for evidence in evidence {
		evidence.verdict = Verdict::Unknown;
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde::de::DeserializeOwned;

	#[derive(Deserialize)]
	struct Case {
		name: String,
		role: Role,
		previous: Option<Fixture>,
		current: Fixture,
		interval: u64,
		age: u64,
		health: serde_json::Value,
	}

	#[derive(Deserialize)]
	struct Fixture {
		observer: Named,
		snapshot: serde_json::Value,
	}

	#[derive(Deserialize)]
	struct Named {
		path: String,
		epoch: Option<String>,
	}

	fn cases() -> Vec<Case> {
		serde_json::from_str(include_str!("../fixtures/health.json")).unwrap()
	}

	fn report<S: DeserializeOwned>(fixture: &Fixture) -> Report<S> {
		let epoch = fixture.observer.epoch.as_deref().map(|epoch| epoch.parse().unwrap());
		let observer = Observer::new(moq_net::Path::new(&fixture.observer.path).to_owned(), epoch);
		Report::new(observer, serde_json::from_value(fixture.snapshot.clone()).unwrap())
	}

	fn grade<S: DeserializeOwned>(
		case: &Case,
		thresholds: &Thresholds,
		grade: fn(&Sample<'_, S>, &Thresholds) -> Health,
	) -> Health {
		let previous = case.previous.as_ref().map(report::<S>);
		let current = report::<S>(&case.current);
		let mut sample = Sample::new(&current, Duration::from_millis(case.age));
		if let Some(previous) = &previous {
			sample = sample.with_previous(previous, Duration::from_millis(case.interval));
		}
		grade(&sample, thresholds)
	}

	fn health(case: &Case, thresholds: &Thresholds) -> Health {
		match case.role {
			Role::Publisher => grade(case, thresholds, publisher::<()>),
			Role::Viewer => grade(case, thresholds, viewer::<()>),
		}
	}

	/// JSON with every number as a float, so Rust's `1.0` compares equal to JavaScript's `1`.
	fn numeric(value: serde_json::Value) -> serde_json::Value {
		use serde_json::Value;
		match value {
			Value::Number(n) => Value::from(n.as_f64().unwrap()),
			Value::Array(items) => Value::Array(items.into_iter().map(numeric).collect()),
			Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, numeric(v))).collect()),
			other => other,
		}
	}

	#[test]
	fn shared_fixtures_grade_as_recorded() {
		for case in cases() {
			let actual = serde_json::to_value(health(&case, &Thresholds::default())).unwrap();
			assert_eq!(numeric(actual), numeric(case.health.clone()), "{}", case.name);
		}
	}

	#[test]
	fn caller_thresholds_override_the_defaults() {
		let case = cases()
			.into_iter()
			.find(|c| c.name.starts_with("viewer healthy"))
			.unwrap();
		let mut thresholds = Thresholds::default();
		thresholds.transport.rtt.degraded = Duration::from_millis(50);
		let health = health(&case, &thresholds);
		assert_eq!(health.transport.verdict, Verdict::Degraded);
		assert_eq!(health.verdict, Verdict::Degraded);
	}

	#[test]
	fn counters_near_the_limit_saturate() {
		let snapshot = |frames: u64, bytes| {
			let rendition = stats::Rendition {
				frames,
				bytes,
				skipped: frames.min(1),
				..Default::default()
			};
			stats::Snapshot::<()> {
				transport: stats::Transport {
					rate: Some(1_000_000),
					lost_bytes: Some(0),
					..Default::default()
				},
				renditions: BTreeMap::from([
					("0.avc3".to_string(), rendition.clone()),
					("1.mp4a".to_string(), rendition),
				]),
				..Default::default()
			}
		};
		let observer = Observer::new(moq_net::Path::new("room/live").to_owned(), None);
		let previous = Report::new(observer.clone(), snapshot(0, 0));
		let current = Report::new(observer, snapshot(u64::MAX, u64::MAX));
		let sample = Sample::new(&current, Duration::ZERO).with_previous(&previous, Duration::from_secs(1));
		let health = publisher(&sample, &Thresholds::default());
		assert_eq!(health.renditions["0.avc3"].bytes, Some(u64::MAX));
		assert_eq!(health.transport.evidence[2].input, Input::Headroom);
		assert_eq!(health.transport.evidence[2].verdict, Verdict::Unhealthy);
	}
}

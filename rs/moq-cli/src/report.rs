//! Media reports: the track `import --report` names in its catalog's `stats` section, the feedback
//! `export --echo` publishes where a catalog solicits it, and the `export report` and
//! `export echo` sinks that print them.
//!
//! Each report is one compressed snapshot track, written about once a second while someone
//! subscribes. See the hang draft's media statistics section for the wire.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_mux::catalog::{CatalogFormat, Stream};
use moq_mux::container::ts;
use moq_tokio::connection::Monitor;
use tokio::io::AsyncWriteExt;

/// How often a report is written: about once a second, as the hang draft asks of a producer.
const INTERVAL: Duration = Duration::from_secs(1);

/// The publisher snapshot track an import names in its catalog's `stats` section.
pub const STATS_TRACK: &str = "stats.json";

/// Lines `export echo` queues for stdout before its readers wait for it to catch up.
const LINES: usize = 64;

/// The most reconstructed state a sink holds for one report. A report is a few KiB, so this
/// only stops a publisher whose patches keep adding keys.
const MAX_REPORT: usize = 1024 * 1024;

/// How long `export echo` lets a reader drain after its broadcast ends. The final snapshot can
/// still be in flight, but the track can also outlive its route.
const DRAIN: Duration = Duration::from_secs(5);

/// Below every media track, so a report never delays the media it describes.
const PRIORITY: i8 = i8::MIN;

/// A compressed snapshot track, `None` once a write failed and closed it.
struct ReportTrack<T> {
	track: Option<moq_json::snapshot::Producer<T>>,
}

impl<T: serde::Serialize> ReportTrack<T> {
	/// Create `name` on `broadcast`.
	fn create(broadcast: &mut moq_net::broadcast::Producer, name: &str) -> anyhow::Result<Self> {
		// Deltas compress against the snapshot in their group, and a joiner asks for a fresh group
		// once they outweigh eight snapshots.
		let info = moq_net::track::Info::default()
			.with_priority(PRIORITY)
			.with_compress(true)
			.with_refresh(8);
		let track = broadcast.create_track(name, info)?;
		Ok(Self {
			track: Some(moq_json::snapshot::Producer::new(track, Default::default())),
		})
	}

	/// Publish `value`, unless nobody subscribes.
	fn update(&mut self, value: &T) {
		if self
			.track
			.as_ref()
			.is_some_and(|track| track.demand().wanted().is_some())
		{
			self.write(value);
		}
	}

	/// Publish `value`. A track that refuses a write would refuse every later one, so it is closed
	/// with the cause, which its subscribers then read, and reported once.
	///
	/// Also restates the value in a fresh group when a joiner wants one. Reports are written every
	/// interval, so checking here answers a joiner within one without a task of its own.
	fn write(&mut self, value: &T) {
		let Some(track) = &mut self.track else {
			return;
		};
		let refreshed = match track.poll_refresh(&moq_net::kio::Waiter::noop()) {
			std::task::Poll::Ready(Err(err)) => Err(err),
			_ => Ok(()),
		};
		if let Err(err) = refreshed.and_then(|()| track.update(value)) {
			tracing::warn!(%err, "a report track failed; closing it");
			if let Some(track) = self.track.take() {
				let _ = track.abort(moq_net::Error::Transport(err.to_string()));
			}
		}
	}

	/// End the track cleanly, keeping the last snapshot.
	fn finish(&mut self) {
		if let Some(track) = &mut self.track
			&& let Err(err) = track.finish()
		{
			tracing::warn!(%err, "failed to finish a report");
		}
	}

	/// Close the track with `err`, so subscribers read why the report ended.
	fn abort(self, err: moq_net::Error) {
		if let Some(track) = self.track {
			let _ = track.abort(err);
		}
	}
}

/// Which way the reported media crosses the connection, which picks its rate estimate.
#[derive(Clone, Copy)]
enum Direction {
	Send,
	Receive,
}

/// The connection's transport measurements now, or none without a dialed connection or while
/// it reconnects.
fn transport(connection: Option<&Monitor>, direction: Direction) -> hang::stats::Transport {
	let mut transport = hang::stats::Transport::default();
	if let Some(stats) = connection.and_then(Monitor::stats) {
		let rate = match direction {
			Direction::Send => stats.estimated_send_rate,
			// A receive estimate costs PROBE traffic, which an export does not request just to report.
			Direction::Receive => None,
		};
		transport.rtt = stats.rtt;
		transport.rate = rate.map(moq_net::bandwidth::Rate::as_bps);
		transport.lost_bytes = stats.bytes_lost;
		transport.lost_packets = stats.packets_lost;
	}
	transport
}

/// The MPEG-TS sections a report carries beside its media counters, as the import or export
/// loop that samples them last set them. Empty for every other container.
#[derive(Clone, Default)]
pub struct Sections(Arc<Mutex<ts::stats::Report>>);

impl Sections {
	pub fn set(&self, update: impl FnOnce(&mut ts::stats::Report)) {
		update(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
	}

	fn get(&self) -> ts::stats::Report {
		self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
	}
}

/// An import's stats track: what each of its tracks has written, from its catalog's counters,
/// the connection's transport, and any MPEG-TS sections.
pub struct Stats {
	tracks: ReportTrack<hang::stats::Snapshot<ts::stats::Report>>,
	renditions: Box<dyn Fn() -> BTreeMap<String, hang::stats::Rendition> + Send>,
	connection: Option<Monitor>,
	sections: Sections,
}

impl Stats {
	/// Create the stats track on `broadcast` and name it in `catalog`'s `stats` section.
	///
	/// Call it while an importer still holds its reservation, so the section rides the first
	/// catalog rather than announcing a catalog with no tracks.
	pub fn new<E: moq_mux::catalog::hang::CatalogExt>(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: &moq_mux::catalog::Producer<E>,
		connection: Option<Monitor>,
	) -> anyhow::Result<Self> {
		let tracks = ReportTrack::create(broadcast, STATS_TRACK)?;
		let mut catalog = catalog.clone();
		let mut guard = catalog.modify()?;
		guard.stats = Some(hang::catalog::Stats::new(STATS_TRACK));
		guard.commit()?;
		Ok(Self {
			tracks,
			renditions: Box::new(move || catalog.stats()),
			connection,
			sections: Sections::default(),
		})
	}

	/// Where the import sets its MPEG-TS sections.
	pub fn sections(&self) -> Sections {
		self.sections.clone()
	}

	fn snapshot(&self) -> hang::stats::Snapshot<ts::stats::Report> {
		let mut snapshot = hang::stats::Snapshot::default();
		snapshot.transport = transport(self.connection.as_ref(), Direction::Send);
		snapshot.renditions = (self.renditions)();
		snapshot.ext = self.sections.get();
		snapshot
	}

	fn write(&mut self) {
		let snapshot = self.snapshot();
		self.tracks.update(&snapshot);
	}

	/// Write a snapshot every interval, for as long as the import runs beside it.
	pub async fn run(&mut self) -> Infallible {
		let mut interval = moq_clock::interval(INTERVAL);
		loop {
			interval.tick().await;
			self.write();
		}
	}

	/// Write the final counts, subscribed or not, and end the track. Hold on to it while the
	/// import lingers, so a late subscriber still reads them.
	pub fn finish(&mut self) {
		let last = self.snapshot();
		self.tracks.write(&last);
		self.tracks.finish();
	}

	/// End the track with the import's failure, as its media tracks end.
	pub fn abort(self, err: moq_net::Error) {
		self.tracks.abort(err);
	}
}

/// An `export --echo` name: the one path segment a feedback broadcast is published at, before
/// the `.echo` suffix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EchoName(String);

impl std::str::FromStr for EchoName {
	type Err = String;

	fn from_str(name: &str) -> Result<Self, Self::Err> {
		if name.is_empty() || name.contains('/') || matches!(name, "." | ".." | "*" | "**") {
			return Err(format!("`{name}` is not a single path segment"));
		}
		if name.starts_with('.') {
			return Err(format!(
				"`{name}` starts with `.`, which hides its feedback from discovery"
			));
		}
		Ok(Self(name.to_string()))
	}
}

/// Where an `export --echo` publishes its feedback, under what name, and over which connection.
pub struct EchoTarget {
	/// The origin the feedback broadcast is published into.
	pub origin: moq_net::origin::Producer,
	/// The path segment the feedback is published at, before `.echo`.
	pub name: EchoName,
	/// The connection whose transport the feedback reports, when one was dialed.
	pub connection: Option<Monitor>,
}

/// The feedback broadcast in force: where it is published and its snapshot tracks.
struct Feedback {
	path: moq_net::Path,
	// Held for the route: dropping it retracts the announcement.
	_broadcast: moq_net::origin::Publication,
	tracks: ReportTrack<hang::echo::Snapshot<ts::stats::Report>>,
}

/// An export's solicited feedback: once its catalog carries an `echo` section, a
/// `<prefix>/<name>.echo` broadcast whose `feedback.json` reports what each rendition the export
/// reads has received, the connection's transport, and any MPEG-TS sections. Each catalog update
/// reconciles it: a removed section or a changed path retracts the broadcast it replaces. It
/// follows the catalog of the instance the export first reads, and ends when the export does.
pub struct Echo {
	target: EchoTarget,
	source: moq_mux::Source,
	format: CatalogFormat,
	sections: Sections,
	feedback: Option<Feedback>,
}

impl Echo {
	/// Feedback at `target` for the export reading `source` through a `format` catalog.
	pub fn new(target: EchoTarget, source: moq_mux::Source, format: CatalogFormat) -> Self {
		Self {
			target,
			source,
			format,
			sections: Sections::default(),
			feedback: None,
		}
	}

	/// Where the export sets its MPEG-TS sections.
	pub fn sections(&self) -> Sections {
		self.sections.clone()
	}

	/// Follow the catalog's `echo` section and write a snapshot every interval, for as long as the
	/// export runs beside it.
	pub async fn run(&mut self) -> Infallible {
		let mut catalog = match self.source.catalog::<()>(self.format).await {
			Ok(catalog) => Some(catalog),
			Err(err) => {
				tracing::warn!(%err, "no catalog to read a feedback solicitation from");
				None
			}
		};
		let mut interval = moq_clock::interval(INTERVAL);
		loop {
			tokio::select! {
				update = next(&mut catalog) => match update {
					Ok(Some(update)) => self.reconcile(update.echo.as_ref()),
					// The export still writes what it has read once the catalog ends, so the
					// feedback in force stays up until the export finishes it.
					Ok(None) => catalog = None,
					Err(err) => {
						tracing::warn!(%err, "stopped reading the catalog's feedback solicitation");
						catalog = None;
					}
				},
				_ = interval.tick() => self.write(),
			}
		}
	}

	/// Publish at the path `echo` resolves to, retracting a broadcast at any other.
	fn reconcile(&mut self, echo: Option<&hang::catalog::Echo>) {
		let target = echo.and_then(|echo| {
			let prefix = self.source.resolve_reference(Some(&echo.path));
			if prefix.is_none() {
				tracing::warn!(path = %echo.path.as_str(), "the catalog solicits feedback above the connection root");
			}
			let prefix = prefix?;
			let name = format!("{}{}", self.target.name.0, hang::echo::SUFFIX);
			hang::moq_net::Path::new(&name)
				.and_then(|name| prefix.join(&name))
				.inspect_err(|err| tracing::warn!(%err, path = %echo.path.as_str(), "the feedback path is too long"))
				.ok()
		});
		if self.feedback.as_ref().map(|feedback| &feedback.path) == target.as_ref() {
			return;
		}
		self.retire();
		let Some(path) = target else {
			return;
		};
		match self.publish(&path) {
			Ok(feedback) => {
				tracing::info!(path = %path.as_str(), "publishing feedback");
				self.feedback = Some(feedback);
				self.write();
			}
			Err(err) => tracing::warn!(%err, path = %path.as_str(), "failed to publish feedback"),
		}
	}

	fn publish(&self, path: &moq_net::Path) -> anyhow::Result<Feedback> {
		let broadcast = self.target.origin.create_broadcast(path)?;
		let tracks = ReportTrack::create(&mut broadcast.broadcast().clone(), hang::echo::TRACK)?;
		// A fresh instance, so a reader sees a replaced feedback broadcast as a restart.
		broadcast.announce(moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()))?;
		Ok(Feedback {
			path: path.clone(),
			_broadcast: broadcast,
			tracks,
		})
	}

	fn write(&mut self) {
		let Some(feedback) = &mut self.feedback else {
			return;
		};
		let mut snapshot = hang::echo::Snapshot::default();
		snapshot.transport = transport(self.target.connection.as_ref(), Direction::Receive);
		snapshot.renditions = self.source.stats();
		snapshot.ext = self.sections.get();
		feedback.tracks.update(&snapshot);
	}

	/// End the feedback broadcast in force, if any.
	fn retire(&mut self) {
		if let Some(mut feedback) = self.feedback.take() {
			feedback.tracks.finish();
		}
	}

	/// Write the final counts and retract the feedback broadcast.
	pub fn finish(mut self) {
		self.write();
		self.retire();
	}
}

/// The next catalog update, or never once there is no catalog to read.
async fn next<S: Stream>(catalog: &mut Option<S>) -> moq_mux::Result<Option<moq_mux::catalog::hang::Catalog<S::Ext>>> {
	match catalog {
		Some(catalog) => catalog.next().await,
		None => std::future::pending().await,
	}
}

/// Read a report track, holding at most [`MAX_REPORT`] of it.
fn reader(subscriber: moq_net::track::Subscriber) -> moq_json::snapshot::Consumer<serde_json::Value> {
	let mut config = moq_json::snapshot::consumer::Config::default();
	config.max_size = Some(MAX_REPORT);
	moq_json::snapshot::Consumer::new(subscriber, config)
}

/// The next snapshot of a report track, or never while there is none.
async fn snapshot(
	track: &mut Option<moq_json::snapshot::Consumer<serde_json::Value>>,
) -> moq_json::Result<Option<serde_json::Value>> {
	match track {
		Some(track) => Ok(track.latest().await?.map(|timed| timed.value)),
		None => std::future::pending().await,
	}
}

/// `export report`: write the broadcast's media reports to stdout as JSON lines, following the
/// track its catalog's `stats` section names, until the catalog and that track have both ended.
pub async fn print_report(origin: moq_net::origin::Consumer, path: String) -> anyhow::Result<()> {
	let mut stdout = tokio::io::stdout();
	let broadcast = origin
		.follow(path.as_str(), None)?
		.available()
		.await
		.with_context(|| format!("failed to resolve broadcast `{path}`"))?;
	let source = moq_mux::Source::new(origin, broadcast.clone());
	let mut catalog = Some(source.catalog::<()>(CatalogFormat::Hang).await?);

	let mut named = None;
	let mut track = None;
	loop {
		tokio::select! {
			update = next(&mut catalog) => {
				// A finished import ends its stats track just before its catalog: read it out.
				let Some(update) = update? else {
					if track.is_none() {
						return Ok(());
					}
					catalog = None;
					continue;
				};
				let stats = update.stats.map(|stats| stats.track);
				if stats != named {
					track = match &stats {
						Some(name) => {
							let subscriber = broadcast.track(name)?.subscribe(None).await?;
							Some(reader(subscriber))
						}
						None => None,
					};
					named = stats;
				}
			}
			value = snapshot(&mut track) => match value? {
				Some(value) => {
					stdout.write_all(format!("{value}\n").as_bytes()).await?;
					stdout.flush().await?;
				}
				None if catalog.is_none() => return Ok(()),
				None => track = None,
			},
		}
	}
}

/// One line of `export echo`: the feedback broadcast and its latest snapshot.
#[derive(serde::Serialize)]
struct EchoLine<'a> {
	broadcast: &'a str,
	feedback: serde_json::Value,
}

/// `export echo`: write the feedback of every `.echo` broadcast under `prefix` to stdout as JSON
/// lines, following them as they start and end.
pub async fn print_echo(origin: moq_net::origin::Consumer, prefix: String) -> anyhow::Result<()> {
	write_echo(origin, prefix, tokio::io::stdout()).await
}

/// [`print_echo`] to any writer.
async fn write_echo(
	origin: moq_net::origin::Consumer,
	prefix: String,
	mut stdout: impl tokio::io::AsyncWrite + Unpin,
) -> anyhow::Result<()> {
	// Announcements leave hidden routes out unless asked, and a prefix naming one asks.
	let hidden = prefix.split('/').any(|segment| segment.starts_with('.'));
	let prefix = moq_net::Path::new(&prefix)?;
	let mut announced = origin.clone().with_hidden(hidden).announced();
	let (lines, mut written) = tokio::sync::mpsc::channel::<String>(LINES);
	let mut readers = Readers::default();
	loop {
		tokio::select! {
			event = announced.next() => {
				let event = event.context("announcements ended")?;
				let (announce, active) = match event {
					// A restart is another publisher instance, which a reader still on the old one never sees.
					moq_net::announce::Event::Start(announce) | moq_net::announce::Event::Restart(announce) => {
						(announce, true)
					}
					moq_net::announce::Event::End(announce) => (announce, false),
					_ => continue,
				};
				let path = announce.prefix;
				if !path.has_prefix(&prefix) || !hang::echo::is_echo(&path) {
					continue;
				}
				if active {
					let reader = tokio::spawn(read_echo(origin.clone(), path.clone(), lines.clone()));
					readers.start(path, reader);
				} else {
					readers.end(path);
				}
			}
			Some(line) = written.recv() => {
				stdout.write_all(line.as_bytes()).await?;
				stdout.flush().await?;
			}
			() = readers.reap() => {}
		}
	}
}

/// The `export echo` reader of each feedback broadcast, live or draining after its `End`.
#[derive(Default)]
struct Readers {
	running: HashMap<moq_net::Path, tokio::task::JoinHandle<()>>,
	/// One timer per `End`, yielding the path and the reader it ended once `DRAIN` passes.
	drains: tokio::task::JoinSet<(moq_net::Path, tokio::task::Id)>,
}

impl Readers {
	/// Read `path` with `reader`, replacing any reader still on it.
	fn start(&mut self, path: moq_net::Path, reader: tokio::task::JoinHandle<()>) {
		if let Some(previous) = self.running.insert(path, reader) {
			previous.abort();
		}
	}

	/// Let the reader of `path` drain, not abort it: the final snapshot can still be in flight. It
	/// stays so a start at the path replaces it, and goes after `DRAIN` even if its track stays open.
	fn end(&mut self, path: moq_net::Path) {
		let Some(reader) = self.running.get(&path) else {
			return;
		};
		let id = reader.id();
		self.drains.spawn(async move {
			moq_clock::sleep(DRAIN).await;
			(path, id)
		});
	}

	/// Wait for the next drain to pass, then abort and forget its reader unless a start has since
	/// replaced it. Never returns while none is draining.
	async fn reap(&mut self) {
		let (path, id) = loop {
			match self.drains.join_next().await {
				Some(Ok(drained)) => break drained,
				// A timer only sleeps, so it never fails.
				Some(Err(_)) => continue,
				None => std::future::pending().await,
			}
		};
		if self.running.get(&path).is_some_and(|reader| reader.id() == id)
			&& let Some(reader) = self.running.remove(&path)
		{
			reader.abort();
		}
	}
}

/// Send a line for each feedback snapshot the broadcast at `path` publishes, until it ends.
async fn read_echo(origin: moq_net::origin::Consumer, path: moq_net::Path, lines: tokio::sync::mpsc::Sender<String>) {
	let read = async {
		let broadcast = origin.request(&path, None).await?;
		let subscriber = broadcast.track(hang::echo::TRACK)?.subscribe(None).await?;
		let mut track = reader(subscriber);
		while let Some(moq_net::Timed { value: feedback, .. }) = track.latest().await? {
			let line = EchoLine {
				broadcast: path.as_str(),
				feedback,
			};
			if lines
				.send(format!("{}\n", serde_json::to_string(&line)?))
				.await
				.is_err()
			{
				break;
			}
		}
		anyhow::Ok(())
	};
	if let Err(err) = read.await {
		tracing::debug!(%err, path = %path.as_str(), "stopped reading feedback");
	}
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod tests {
	use super::*;
	use tokio::io::{AsyncBufReadExt, BufReader};

	/// Patches that grow a report past `MAX_REPORT` end the read instead of growing the sink.
	#[tokio::test]
	async fn a_report_past_the_cap_ends_the_read() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track(STATS_TRACK, None)
			.unwrap();
		let mut read = reader(track.subscribe(None));
		let mut report = moq_json::snapshot::Producer::<serde_json::Value>::new(track, Default::default());
		report
			.update(&serde_json::json!({ "pad": "x".repeat(MAX_REPORT) }))
			.unwrap();
		assert!(matches!(read.next().await, Err(moq_json::Error::TooLarge(MAX_REPORT))));
	}

	/// A joiner once the track's deltas outweigh its refresh ratio gets the report restated in a
	/// fresh group on the next write.
	#[tokio::test]
	async fn a_late_joiner_gets_a_fresh_report_group() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut report = ReportTrack::<serde_json::Value>::create(&mut broadcast, STATS_TRACK).unwrap();
		let track = report.track.clone().unwrap();
		let _early = track.consume();
		for n in 0..100 {
			report.write(&serde_json::json!({ "n": n }));
		}

		let mut late = track.consume();
		assert_eq!(track.demand().wanted(), Some(1), "the join asks for a fresh group");
		report.write(&serde_json::json!({ "n": 100 }));
		assert_eq!(late.largest().await.map(|position| position.group), Some(1));
	}

	/// A feedback track can outlive its route, so a reader left to drain stops after `DRAIN`.
	#[tokio::test(start_paused = true)]
	async fn an_ended_feedback_reader_stops_after_draining() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("viewer.echo").unwrap();
		let track = broadcast.create_track(hang::echo::TRACK, None).unwrap();
		let mut feedback = moq_json::snapshot::Producer::<serde_json::Value>::new(track, Default::default());
		broadcast.announce(Default::default()).unwrap();

		let (out, read) = tokio::io::duplex(4096);
		let mut lines = BufReader::new(read).lines();
		tokio::spawn(write_echo(origin.consume(), String::new(), out));

		feedback.update(&serde_json::json!({ "n": 1 })).unwrap();
		let line = lines.next_line().await.unwrap().unwrap();
		assert!(line.contains(r#""n":1"#), "{line}");

		broadcast.unannounce();
		tokio::time::sleep(DRAIN * 2).await;
		feedback.update(&serde_json::json!({ "n": 2 })).unwrap();
		let late = tokio::time::timeout(Duration::from_secs(1), lines.next_line()).await;
		assert!(late.is_err(), "a drained reader wrote {late:?}");
	}

	/// A reader is forgotten once its drain passes, unless a start at its path replaced it.
	#[tokio::test(start_paused = true)]
	async fn a_drained_reader_is_forgotten() {
		let mut readers = Readers::default();
		let path = moq_net::Path::from_static("viewer.echo");

		readers.start(path.clone(), tokio::spawn(std::future::pending()));
		readers.end(path.clone());
		readers.reap().await;
		assert!(readers.running.is_empty());

		readers.start(path.clone(), tokio::spawn(std::future::pending()));
		readers.end(path.clone());
		let replacement = tokio::spawn(std::future::pending());
		let id = replacement.id();
		readers.start(path.clone(), replacement);
		readers.reap().await;
		assert_eq!(readers.running.get(&path).map(|reader| reader.id()), Some(id));
	}

	/// Feedback under a hidden prefix is found when that prefix is the one asked for.
	#[tokio::test(start_paused = true)]
	async fn a_hidden_prefix_finds_its_feedback() {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast(".viewers/alice.echo").unwrap();
		let track = broadcast.create_track(hang::echo::TRACK, None).unwrap();
		let mut feedback = moq_json::snapshot::Producer::<serde_json::Value>::new(track, Default::default());
		broadcast.announce(Default::default()).unwrap();

		let (out, read) = tokio::io::duplex(4096);
		let mut lines = BufReader::new(read).lines();
		tokio::spawn(write_echo(origin.consume(), ".viewers".to_string(), out));

		feedback.update(&serde_json::json!({ "n": 1 })).unwrap();
		let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await;
		let line = line.expect("no feedback read").unwrap().unwrap();
		assert!(line.contains(r#""n":1"#), "{line}");
	}
}

#[cfg(test)]
#[path = "report/echo_test.rs"]
#[expect(clippy::disallowed_methods, reason = "tests control the clock directly")]
mod echo_test;

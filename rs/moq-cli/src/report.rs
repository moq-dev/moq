//! Media reports: the stats track `import --stats` names in its catalog, the feedback
//! `export --echo` publishes where a catalog solicits it, and the `export stats` and
//! `export echo` sinks that print them.
//!
//! Each report is one snapshot track and its compressed `.z` sibling, written about once a
//! second while someone subscribes. See the hang draft's media statistics section for the wire.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_mux::catalog::{CatalogFormat, Stream};
use moq_tokio::connection::Monitor;
use tokio::io::AsyncWriteExt;

/// How often a report is written: about once a second, as the hang draft asks of a producer.
const INTERVAL: Duration = Duration::from_secs(1);

/// The publisher snapshot track an import names in its catalog's `stats` section.
pub const STATS_TRACK: &str = "stats.json";

/// The suffix naming a snapshot track's compressed sibling.
const COMPRESSED: &str = ".z";

/// Below every media track, so a report never delays the media it describes.
const PRIORITY: u8 = 0;

/// A snapshot track and its compressed `.z` sibling, written together.
struct Pair<T> {
	plain: moq_json::snapshot::Producer<T>,
	compressed: moq_json::snapshot::Producer<T>,
}

impl<T: serde::Serialize> Pair<T> {
	/// Create `name` and `name.z` on `broadcast`.
	fn create(broadcast: &mut moq_net::broadcast::Producer, name: &str) -> anyhow::Result<Self> {
		let info = moq_net::track::Info::default().with_priority(PRIORITY);
		let plain = broadcast.create_track(name, info.clone())?;
		let compressed = broadcast.create_track(format!("{name}{COMPRESSED}").as_str(), info)?;
		let mut deflate = moq_json::snapshot::Config::default();
		deflate.compression = moq_json::Compression::Deflate;
		Ok(Self {
			plain: moq_json::snapshot::Producer::new(plain, moq_json::snapshot::Config::default().with_delta_ratio(0)),
			compressed: moq_json::snapshot::Producer::new(compressed, deflate),
		})
	}

	/// Publish `value` on both tracks, unless nobody subscribes to either.
	fn update(&mut self, value: &T) {
		if self.plain.demand().is_used() || self.compressed.demand().is_used() {
			self.write(value);
		}
	}

	/// Publish `value` on both tracks.
	fn write(&mut self, value: &T) {
		for track in [&mut self.plain, &mut self.compressed] {
			if let Err(err) = track.update(value) {
				tracing::debug!(%err, "failed to write a report");
			}
		}
	}

	/// End both tracks cleanly, keeping the last snapshot.
	fn finish(&mut self) {
		for track in [&mut self.plain, &mut self.compressed] {
			if let Err(err) = track.finish() {
				tracing::debug!(%err, "failed to finish a report");
			}
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
			Direction::Receive => stats.estimated_recv_rate,
		};
		transport.rtt = stats.rtt;
		transport.rate = rate.map(moq_net::bandwidth::Rate::as_bps);
		transport.lost_bytes = stats.bytes_lost;
		transport.lost_packets = stats.packets_lost;
	}
	transport
}

/// An import's stats track: what each of its tracks has written, from its catalog's counters,
/// and the connection's transport.
pub struct Stats {
	tracks: Pair<hang::stats::Snapshot>,
	renditions: Box<dyn Fn() -> BTreeMap<String, hang::stats::Rendition> + Send>,
	connection: Option<Monitor>,
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
		let tracks = Pair::create(broadcast, STATS_TRACK)?;
		let mut catalog = catalog.clone();
		let mut guard = catalog.modify()?;
		guard.stats = Some(hang::catalog::Stats::new(STATS_TRACK));
		guard.commit()?;
		Ok(Self {
			tracks,
			renditions: Box::new(move || catalog.stats()),
			connection,
		})
	}

	fn snapshot(&self) -> hang::stats::Snapshot {
		let mut snapshot = hang::stats::Snapshot::default();
		snapshot.transport = transport(self.connection.as_ref(), Direction::Send);
		snapshot.renditions = (self.renditions)();
		snapshot
	}

	fn write(&mut self) {
		let snapshot = self.snapshot();
		self.tracks.update(&snapshot);
	}

	/// Write a snapshot every interval, for as long as the import runs beside it.
	pub async fn run(&mut self) -> Infallible {
		let mut interval = tokio::time::interval(INTERVAL);
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
		Ok(Self(name.to_string()))
	}
}

/// The feedback broadcast in force: where it is published and its snapshot tracks.
struct Feedback {
	path: moq_net::PathOwned,
	// Held for the route: dropping it retracts the announcement.
	_broadcast: moq_net::broadcast::Producer,
	tracks: Pair<hang::echo::Snapshot>,
}

/// An export's solicited feedback: once its catalog carries an `echo` section, a
/// `<prefix>/<name>.echo` broadcast whose `feedback.json` reports what each rendition the export
/// reads has received, and the connection's transport. Each catalog update reconciles it: a
/// removed section or a changed path retracts the broadcast it replaces. It follows the catalog
/// of the instance the export first reads, and ends when the export does.
pub struct Echo {
	origin: moq_net::origin::Producer,
	source: moq_mux::Source,
	format: CatalogFormat,
	name: EchoName,
	connection: Option<Monitor>,
	feedback: Option<Feedback>,
}

impl Echo {
	/// Feedback for the export reading `source`, published into `origin`.
	pub fn new(
		origin: moq_net::origin::Producer,
		source: moq_mux::Source,
		format: CatalogFormat,
		name: EchoName,
		connection: Option<Monitor>,
	) -> Self {
		Self {
			origin,
			source,
			format,
			name,
			connection,
			feedback: None,
		}
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
		let mut interval = tokio::time::interval(INTERVAL);
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
			Some(prefix?.join(format!("{}{}", self.name.0, hang::echo::SUFFIX).as_str()))
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

	fn publish(&self, path: &moq_net::PathOwned) -> anyhow::Result<Feedback> {
		let mut broadcast = self.origin.create_broadcast(path)?;
		let tracks = Pair::create(&mut broadcast, hang::echo::TRACK)?;
		broadcast.announce(moq_net::origin::Route::default())?;
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
		snapshot.transport = transport(self.connection.as_ref(), Direction::Receive);
		snapshot.renditions = self.source.stats();
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

/// The next snapshot of a report track, or never while there is none.
async fn snapshot(
	track: &mut Option<moq_json::snapshot::Consumer<serde_json::Value>>,
) -> moq_json::Result<Option<serde_json::Value>> {
	match track {
		Some(track) => track.next().await,
		None => std::future::pending().await,
	}
}

/// `export stats`: write the broadcast's stats snapshots to stdout as JSON lines, following the
/// track its catalog's `stats` section names, until the catalog and that track have both ended.
pub async fn print_stats(origin: moq_net::origin::Consumer, path: String) -> anyhow::Result<()> {
	let mut stdout = tokio::io::stdout();
	origin
		.routed(&path)
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast `{path}` was announced"))?;
	let broadcast = origin
		.request_broadcast(path.as_str(), None)
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
							Some(moq_json::snapshot::Consumer::new(subscriber, Default::default()))
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
	let mut stdout = tokio::io::stdout();
	let prefix = moq_net::Path::new(&prefix).to_owned();
	let mut announced = origin.announced();
	let (lines, mut written) = tokio::sync::mpsc::unbounded_channel::<String>();
	let mut readers = HashMap::new();
	loop {
		tokio::select! {
			event = announced.next() => {
				let event = event.context("announcements ended")?;
				let (announce, active) = match event {
					moq_net::announce::Event::Start(announce) => (announce, true),
					moq_net::announce::Event::End(announce) => (announce, false),
					_ => continue,
				};
				let path = announce.prefix;
				if !path.has_prefix(&prefix) || !hang::echo::is_echo(&path) {
					continue;
				}
				if active {
					let reader = tokio::spawn(read_echo(origin.clone(), path.clone(), lines.clone()));
					readers.insert(path, reader);
				} else if let Some(reader) = readers.remove(&path) {
					reader.abort();
				}
			}
			Some(line) = written.recv() => {
				stdout.write_all(line.as_bytes()).await?;
				stdout.flush().await?;
			}
		}
	}
}

/// Send a line for each feedback snapshot the broadcast at `path` publishes, until it ends.
async fn read_echo(
	origin: moq_net::origin::Consumer,
	path: moq_net::PathOwned,
	lines: tokio::sync::mpsc::UnboundedSender<String>,
) {
	let read = async {
		let broadcast = origin.request_broadcast(&path, None).await?;
		let subscriber = broadcast.track(hang::echo::TRACK)?.subscribe(None).await?;
		let mut track = moq_json::snapshot::Consumer::<serde_json::Value>::new(subscriber, Default::default());
		while let Some(feedback) = track.next().await? {
			let line = EchoLine {
				broadcast: path.as_str(),
				feedback,
			};
			if lines.send(format!("{}\n", serde_json::to_string(&line)?)).is_err() {
				break;
			}
		}
		anyhow::Ok(())
	};
	if let Err(err) = read.await {
		tracing::debug!(%err, path = %path.as_str(), "stopped reading feedback");
	}
}

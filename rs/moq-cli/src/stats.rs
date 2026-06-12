use std::{fmt::Write as _, io::Write as _, time::Duration};

use hang::moq_net;

/// CLI flags for the live usage display, flattened into each subcommand.
#[derive(clap::Args, Clone, Debug)]
pub struct StatsArgs {
	/// Print a live, self-refreshing table of per-track upload/download usage
	/// to stderr.
	#[arg(long = "stats")]
	pub enabled: bool,

	/// How often to refresh the usage table.
	#[arg(long = "stats-interval", default_value = "1s", value_parser = humantime::parse_duration)]
	pub interval: Duration,
}

impl Default for StatsArgs {
	fn default() -> Self {
		Self {
			enabled: false,
			interval: Duration::from_secs(1),
		}
	}
}

impl StatsArgs {
	/// Build an enabled aggregator when `--stats` is set, otherwise `None`.
	///
	/// We read counters in-process via [`moq_net::Stats::snapshot`], but the
	/// aggregator still needs an origin to actually collect, so we hand it a
	/// throwaway one. Its published `.stats` broadcast simply goes unconsumed.
	pub fn build(&self) -> Option<moq_net::Stats> {
		if !self.enabled {
			return None;
		}
		let origin = moq_net::Origin::random().produce();
		Some(moq_net::Stats::new(
			moq_net::StatsConfig::new()
				.with_origin(origin)
				.with_interval(self.interval),
		))
	}
}

/// Drive the usage display for the lifetime of a transfer.
///
/// With `--stats` off (`stats` is `None`) this stays pending forever, so it's
/// inert as a `tokio::select!` branch. With it on, it repaints the table every
/// `interval` until the surrounding `select!` resolves on another branch.
pub async fn run_stats(stats: Option<moq_net::Stats>, interval: Duration) -> anyhow::Result<()> {
	match stats {
		Some(stats) => run(stats, interval).await,
		None => std::future::pending().await,
	}
}

/// Render the usage table on an interval until cancelled, repainting in place.
///
/// Never returns `Ok`; it loops forever so callers can drop it into a
/// `tokio::select!` alongside the actual transfer.
async fn run(stats: moq_net::Stats, interval: Duration) -> anyhow::Result<()> {
	let mut prev = stats.snapshot();
	let mut prev_lines = 0usize;

	let mut ticker = tokio::time::interval(interval);
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
	ticker.tick().await; // first tick is immediate

	loop {
		ticker.tick().await;
		let now = stats.snapshot();
		let table = render(&prev, &now, interval);

		let mut out = String::new();
		// Move the cursor back up over the previous table and clear from there
		// to the end of the screen, so the table repaints in place.
		if prev_lines > 0 {
			let _ = write!(out, "\x1b[{prev_lines}A");
		}
		out.push_str("\x1b[0J");
		out.push_str(&table);

		let mut stderr = std::io::stderr().lock();
		let _ = stderr.write_all(out.as_bytes());
		let _ = stderr.flush();

		prev_lines = table.lines().count();
		prev = now;
	}
}

/// One row of the table: a track (or a broadcast total), with its cumulative
/// bytes and the rate since the previous snapshot.
struct Row {
	label: String,
	bytes: u64,
	rate: f64,
	frames: u64,
	groups: u64,
}

fn render(prev: &moq_net::StatsSnapshot, now: &moq_net::StatsSnapshot, interval: Duration) -> String {
	let secs = interval.as_secs_f64().max(f64::MIN_POSITIVE);
	let tier = moq_net::Tier::External.idx();
	let mut rows: Vec<Row> = Vec::new();

	for (path, bc) in &now.broadcasts {
		let prev_bc = prev.broadcasts.get(path);

		// A given moq-cli process only ever uploads (publisher) or downloads
		// (subscriber), so at most one of these sides is populated.
		for (side, prev_side) in [
			(&bc.publisher[tier], prev_bc.map(|b| &b.publisher[tier])),
			(&bc.subscriber[tier], prev_bc.map(|b| &b.subscriber[tier])),
		] {
			if side.bytes == 0 && side.tracks.is_empty() {
				continue;
			}

			rows.push(Row {
				label: path.to_string(),
				bytes: side.bytes,
				rate: rate(side.bytes, prev_side.map(|s| s.bytes), secs),
				frames: side.frames,
				groups: side.groups,
			});

			for (name, track) in &side.tracks {
				let prev_track = prev_side.and_then(|s| s.tracks.get(name));
				rows.push(Row {
					label: format!("  {name}"),
					bytes: track.bytes,
					rate: rate(track.bytes, prev_track.map(|t| t.bytes), secs),
					frames: track.frames,
					groups: track.groups,
				});
			}
		}
	}

	let mut out = String::new();
	if rows.is_empty() {
		out.push_str("track: waiting for traffic...\n");
		return out;
	}

	let label_w = rows.iter().map(|r| r.label.len()).max().unwrap_or(0).max(5);
	let _ = writeln!(
		out,
		"{:<label_w$}  {:>11}  {:>11}  {:>8}  {:>8}",
		"track", "rate", "total", "frames", "groups",
	);
	for r in &rows {
		let _ = writeln!(
			out,
			"{:<label_w$}  {:>9}/s  {:>11}  {:>8}  {:>8}",
			r.label,
			human_bytes(r.rate),
			human_bytes(r.bytes as f64),
			r.frames,
			r.groups,
		);
	}
	out
}

/// Per-second rate between two cumulative readings. A missing or decreased
/// previous value (a counter reset, e.g. a reconnect) counts from zero rather
/// than going negative.
fn rate(now: u64, prev: Option<u64>, secs: f64) -> f64 {
	let delta = now.saturating_sub(prev.unwrap_or(0));
	delta as f64 / secs
}

fn human_bytes(n: f64) -> String {
	const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
	let mut v = n;
	let mut unit = 0;
	while v >= 1000.0 && unit < UNITS.len() - 1 {
		v /= 1000.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{v:.0} {}", UNITS[unit])
	} else {
		format!("{v:.1} {}", UNITS[unit])
	}
}

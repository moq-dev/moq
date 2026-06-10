//! Host overload checks for the relay's `/health` endpoint.
//!
//! Operators configure resource thresholds (`--web-health-cpu`, `-ram`,
//! `-rx`/`-tx`, `-load1`/`5`/`15`) and/or an external health service
//! (`--web-health-api`). A `GET /health` returns `200` when every configured
//! check passes and `503` (with a plain-text list of breached thresholds) when
//! any fails, so an upstream load balancer can shed traffic away from a
//! struggling node. With nothing configured it's a pure liveness probe.
//!
//! Host metrics come from [`sysinfo`], which is cross-platform; load average
//! is Unix-only and those flags don't exist on other targets.

use std::{
	fmt,
	str::FromStr,
	sync::{Arc, Mutex},
	time::Duration,
};

use clap::Args;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Networks, RefreshKind, System};
use url::Url;

/// How long to wait on the external `--web-health-api` before treating it as
/// unreachable (and therefore unhealthy).
const API_TIMEOUT: Duration = Duration::from_secs(5);

/// Default sampling interval (seconds) for CPU/RAM/network metrics.
const DEFAULT_INTERVAL: u64 = 2;

/// Configuration for the relay's `/health` endpoint.
///
/// Every threshold is `Option<T>` so an absent CLI flag doesn't clobber a
/// TOML-configured value during `Config::load`'s `update_from` re-parse (see
/// the "Config flags + TOML merge" note in `CLAUDE.md` and the regression test
/// in `config.rs`). A `None` threshold is simply not enforced.
#[serde_as]
#[derive(Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "health-config")]
pub struct HealthConfig {
	/// Return 503 when global CPU usage exceeds this percentage. Accepts
	/// `75` or `75%`.
	#[arg(long = "web-health-cpu", id = "web-health-cpu", env = "MOQ_WEB_HEALTH_CPU", value_parser = parse_percent)]
	pub cpu: Option<f32>,

	/// Return 503 when memory usage exceeds this limit. Accepts a percentage
	/// of total RAM (`80%`) or an absolute used-bytes amount (`32GB`, `32GiB`).
	#[arg(long = "web-health-ram", id = "web-health-ram", env = "MOQ_WEB_HEALTH_RAM", value_parser = parse_mem)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub ram: Option<MemLimit>,

	/// Return 503 when aggregate received throughput exceeds this rate. A unit
	/// is required; lowercase `b` is bits, uppercase `B` is bytes (`4Gb`,
	/// `500MB`). `/s` is always implied.
	#[arg(long = "web-health-rx", id = "web-health-rx", env = "MOQ_WEB_HEALTH_RX", value_parser = parse_rate)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub rx: Option<Rate>,

	/// Return 503 when aggregate transmitted throughput exceeds this rate.
	/// Same syntax as `--web-health-rx`.
	#[arg(long = "web-health-tx", id = "web-health-tx", env = "MOQ_WEB_HEALTH_TX", value_parser = parse_rate)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub tx: Option<Rate>,

	/// Return 503 when the 1-minute load average exceeds this limit. Accepts a
	/// raw value (`6.0`) or a percentage of CPU cores (`80%`, i.e. a load of
	/// `0.8 * cores`). Unix only.
	#[cfg(unix)]
	#[arg(long = "web-health-load1", id = "web-health-load1", env = "MOQ_WEB_HEALTH_LOAD1", value_parser = parse_load)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub load1: Option<LoadLimit>,

	/// Return 503 when the 5-minute load average exceeds this limit. Same syntax
	/// as `--web-health-load1`. Unix only.
	#[cfg(unix)]
	#[arg(long = "web-health-load5", id = "web-health-load5", env = "MOQ_WEB_HEALTH_LOAD5", value_parser = parse_load)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub load5: Option<LoadLimit>,

	/// Return 503 when the 15-minute load average exceeds this limit. Same syntax
	/// as `--web-health-load1`. Unix only.
	#[cfg(unix)]
	#[arg(long = "web-health-load15", id = "web-health-load15", env = "MOQ_WEB_HEALTH_LOAD15", value_parser = parse_load)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub load15: Option<LoadLimit>,

	/// Defer the health decision to another service. On each request the relay
	/// GETs this URL; a non-2xx response or an unreachable service counts as a
	/// breach (fail closed). Merges with the local thresholds above.
	#[arg(long = "web-health-api", id = "web-health-api", env = "MOQ_WEB_HEALTH_API")]
	pub api: Option<Url>,

	/// Seconds between metric samples. Defaults to 2, floored at 1.
	#[arg(
		long = "web-health-interval",
		id = "web-health-interval",
		env = "MOQ_WEB_HEALTH_INTERVAL"
	)]
	pub interval: Option<u64>,
}

impl HealthConfig {
	/// Build a [`Health`] monitor from this config.
	///
	/// Spawns a background sampler only when a CPU/RAM/network threshold is set
	/// (load average and the external API are read on demand, so they need no
	/// sampler). With no thresholds at all, `/health` is a pure liveness probe.
	pub fn build(&self) -> Health {
		let api = self.api.clone().map(|url| HealthApi {
			client: reqwest::Client::builder()
				.timeout(API_TIMEOUT)
				.build()
				.expect("failed to build health-api client"),
			url,
		});

		// Logical CPU count, used to resolve percentage load-average limits.
		// Respects cgroup/affinity limits where the platform exposes them.
		let cores = std::thread::available_parallelism()
			.map(|n| n.get() as f64)
			.unwrap_or(1.0);

		let inner = Arc::new(HealthInner {
			config: self.clone(),
			sample: Mutex::new(Sample::default()),
			api,
			cores,
		});

		let want_cpu = self.cpu.is_some();
		let want_ram = self.ram.is_some();
		let want_rx = self.rx.is_some();
		let want_tx = self.tx.is_some();
		let want_net = want_rx || want_tx;

		if want_cpu || want_ram || want_net {
			let interval = Duration::from_secs(self.interval.unwrap_or(DEFAULT_INTERVAL).max(1));

			let mut kind = RefreshKind::nothing();
			if want_cpu {
				kind = kind.with_cpu(CpuRefreshKind::nothing().with_cpu_usage());
			}
			if want_ram {
				kind = kind.with_memory(MemoryRefreshKind::nothing().with_ram());
			}

			let inner = inner.clone();
			tokio::spawn(async move {
				let mut system = System::new_with_specifics(kind);
				let mut networks = want_net.then(Networks::new_with_refreshed_list);
				let secs = interval.as_secs_f64();
				let mut ticker = tokio::time::interval(interval);

				loop {
					ticker.tick().await;

					// CPU and network rates are deltas vs the previous tick, so the
					// first sample reads ~0 and self-corrects after one interval.
					if want_cpu || want_ram {
						system.refresh_specifics(kind);
					}

					let (mut rx, mut tx) = (0u64, 0u64);
					if let Some(networks) = networks.as_mut() {
						networks.refresh(true);
						for data in networks.values() {
							rx += data.received();
							tx += data.transmitted();
						}
					}

					let mut sample = inner.sample.lock().unwrap();
					sample.cpu = want_cpu.then(|| system.global_cpu_usage());
					if want_ram {
						sample.ram_used = Some(system.used_memory());
						sample.ram_total = Some(system.total_memory());
					}
					sample.rx = want_rx.then(|| (rx as f64 / secs) as u64);
					sample.tx = want_tx.then(|| (tx as f64 / secs) as u64);
				}
			});
		}

		Health { inner }
	}
}

/// A cheap-to-clone handle to the live health state.
#[derive(Clone)]
pub struct Health {
	inner: Arc<HealthInner>,
}

struct HealthInner {
	config: HealthConfig,
	sample: Mutex<Sample>,
	api: Option<HealthApi>,
	cores: f64,
}

struct HealthApi {
	client: reqwest::Client,
	url: Url,
}

/// The most recent metric sample. Only fields with a configured threshold are
/// populated; the rest stay `None` and are never evaluated.
#[derive(Default, Clone, Copy)]
struct Sample {
	cpu: Option<f32>,
	ram_used: Option<u64>,
	ram_total: Option<u64>,
	rx: Option<u64>,
	tx: Option<u64>,
}

impl Health {
	/// Evaluate the local resource thresholds. Returns one message per breached
	/// threshold; an empty vec means healthy. Does not consult the external API
	/// (that's async, see [`Self::check_api`]).
	pub fn check(&self) -> Vec<String> {
		let sample = *self.inner.sample.lock().unwrap();
		evaluate(&self.inner.config, &sample, self.read_load(), self.inner.cores)
	}

	/// Query the external `--web-health-api`, if configured. Returns a breach
	/// message when the service is unhealthy or unreachable, else `None`.
	pub async fn check_api(&self) -> Option<String> {
		let api = self.inner.api.as_ref()?;
		match api.client.get(api.url.clone()).send().await {
			Ok(resp) if resp.status().is_success() => None,
			Ok(resp) => Some(format!("health-api returned {}", resp.status().as_u16())),
			Err(err) => {
				// `reqwest::Error`'s Display includes the request URL; keep that
				// out of the unauthenticated /health body and log it instead.
				tracing::warn!(error = %err, "health-api probe failed");
				Some("health-api unreachable".to_owned())
			}
		}
	}

	/// Read the load average when a load threshold is configured (Unix only).
	#[cfg(unix)]
	fn read_load(&self) -> (f64, f64, f64) {
		let cfg = &self.inner.config;
		if cfg.load1.is_some() || cfg.load5.is_some() || cfg.load15.is_some() {
			let load = System::load_average();
			(load.one, load.five, load.fifteen)
		} else {
			(0.0, 0.0, 0.0)
		}
	}

	#[cfg(not(unix))]
	fn read_load(&self) -> (f64, f64, f64) {
		(0.0, 0.0, 0.0)
	}
}

/// Pure threshold evaluation, split out so it's testable without sysinfo/tokio.
/// `cores` is the logical CPU count, used to resolve percentage load limits.
fn evaluate(cfg: &HealthConfig, sample: &Sample, load: (f64, f64, f64), cores: f64) -> Vec<String> {
	let mut breaches = Vec::new();

	if let (Some(limit), Some(cpu)) = (cfg.cpu, sample.cpu)
		&& cpu > limit
	{
		breaches.push(format!("cpu {cpu:.1}% exceeds {limit}%"));
	}

	if let (Some(limit), Some(used), Some(total)) = (cfg.ram.as_ref(), sample.ram_used, sample.ram_total) {
		let breach = match limit {
			MemLimit::Percent(p) => {
				let pct = used as f64 / total as f64 * 100.0;
				(pct > *p as f64).then(|| format!("ram {pct:.1}% exceeds {p}%"))
			}
			MemLimit::Bytes(b) => (used > *b).then(|| format!("ram {} exceeds {}", human_bytes(used), human_bytes(*b))),
		};
		breaches.extend(breach);
	}

	if let (Some(limit), Some(rx)) = (cfg.rx.as_ref(), sample.rx)
		&& rx > limit.0
	{
		breaches.push(format!("rx {}/s exceeds {}/s", human_bytes(rx), human_bytes(limit.0)));
	}

	if let (Some(limit), Some(tx)) = (cfg.tx.as_ref(), sample.tx)
		&& tx > limit.0
	{
		breaches.push(format!("tx {}/s exceeds {}/s", human_bytes(tx), human_bytes(limit.0)));
	}

	#[cfg(unix)]
	{
		let (one, five, fifteen) = load;
		for (label, value, limit) in [
			("load1", one, cfg.load1),
			("load5", five, cfg.load5),
			("load15", fifteen, cfg.load15),
		] {
			if let Some(limit) = limit
				&& value > limit.resolve(cores)
			{
				breaches.push(match limit {
					// Report the breach in the same units the operator configured.
					LoadLimit::Absolute(t) => format!("{label} {value:.2} exceeds {t:.2}"),
					LoadLimit::Percent(p) => format!("{label} {:.1}% exceeds {p}%", value / cores * 100.0),
				});
			}
		}
	}
	#[cfg(not(unix))]
	let _ = (load, cores);

	breaches
}

/// A memory threshold: a percentage of total RAM, or an absolute byte count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MemLimit {
	Percent(f32),
	Bytes(u64),
}

impl FromStr for MemLimit {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let s = s.trim();
		match s.strip_suffix('%') {
			Some(pct) => {
				let value: f32 = pct.trim().parse().map_err(|_| format!("invalid percentage: '{s}'"))?;
				check_percent(value, s).map(MemLimit::Percent)
			}
			// Bits make no sense for memory, so only accept byte units.
			None => parse_size(s, false).map(MemLimit::Bytes),
		}
	}
}

impl fmt::Display for MemLimit {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			// Both forms round-trip back through `FromStr` (a bare `<n>B` parses
			// as bytes with no prefix).
			MemLimit::Percent(p) => write!(f, "{p}%"),
			MemLimit::Bytes(b) => write!(f, "{b}B"),
		}
	}
}

/// A throughput threshold in bytes per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate(pub u64);

impl FromStr for Rate {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		// Bits allowed: `4Gb` is gigabits/s, `500MB` is megabytes/s.
		parse_size(s.trim(), true).map(Rate)
	}
}

impl fmt::Display for Rate {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}B", self.0)
	}
}

/// A load-average threshold: a raw value, or a percentage of CPU cores.
///
/// `Percent(80.0)` resolves to a load of `0.8 * cores`, so `100%` is "one
/// runnable task per core on average".
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoadLimit {
	Absolute(f64),
	Percent(f64),
}

#[cfg(unix)]
impl LoadLimit {
	/// The raw load-average value this limit corresponds to, given `cores`.
	fn resolve(&self, cores: f64) -> f64 {
		match self {
			LoadLimit::Absolute(v) => *v,
			LoadLimit::Percent(p) => p / 100.0 * cores,
		}
	}
}

#[cfg(unix)]
impl FromStr for LoadLimit {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let s = s.trim();
		// A load can exceed core count, so percentages aren't capped at 100
		// (unlike CPU/RAM). Only reject negatives, which would make every real
		// load average breach and pin the host at 503.
		let (raw, build): (&str, fn(f64) -> LoadLimit) = match s.strip_suffix('%') {
			Some(pct) => (pct.trim(), LoadLimit::Percent),
			None => (s, LoadLimit::Absolute),
		};
		let value: f64 = raw.parse().map_err(|_| format!("invalid load value: '{s}'"))?;
		if value < 0.0 {
			return Err(format!("load limit cannot be negative: '{s}'"));
		}
		Ok(build(value))
	}
}

#[cfg(unix)]
impl fmt::Display for LoadLimit {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			LoadLimit::Absolute(v) => write!(f, "{v}"),
			LoadLimit::Percent(p) => write!(f, "{p}%"),
		}
	}
}

#[cfg(unix)]
fn parse_load(s: &str) -> Result<LoadLimit, String> {
	LoadLimit::from_str(s)
}

/// Parse a CPU/RAM percentage, accepting an optional trailing `%` (`75` or
/// `75%`). The value is in percent units, not a 0-1 fraction, and must be in
/// `0..=100` (a utilization gauge can't exceed 100%, so a typo'd `150` is a
/// config error rather than a silently-disabled check).
fn parse_percent(s: &str) -> Result<f32, String> {
	let s = s.trim();
	let digits = s.strip_suffix('%').unwrap_or(s).trim();
	let value: f32 = digits.parse().map_err(|_| format!("invalid percentage: '{s}'"))?;
	check_percent(value, s)
}

/// Bounds a utilization percentage to `0..=100`.
fn check_percent(value: f32, raw: &str) -> Result<f32, String> {
	if !(0.0..=100.0).contains(&value) {
		return Err(format!("percentage must be between 0 and 100: '{raw}'"));
	}
	Ok(value)
}

fn parse_mem(s: &str) -> Result<MemLimit, String> {
	MemLimit::from_str(s)
}

fn parse_rate(s: &str) -> Result<Rate, String> {
	Rate::from_str(s)
}

/// Parse a byte (or, when `bits_allowed`, bit) size into bytes.
///
/// Grammar: `<number><prefix><b|B>?`. A unit is required (a bare number is
/// rejected, so `80` can't silently mean 80 bytes). The prefix is SI by default
/// (`k`/`M`/`G`/`T` = 1000-based) or binary with an `i` (`Ki`/`Mi`/`Gi`/`Ti` =
/// 1024-based). A trailing lowercase `b` means bits (divided by 8); uppercase
/// `B` (or no suffix) means bytes.
fn parse_size(s: &str, bits_allowed: bool) -> Result<u64, String> {
	let split = s
		.find(|c: char| c.is_ascii_alphabetic())
		.ok_or_else(|| format!("missing unit in '{s}' (e.g. 500MB or 4Gb)"))?;
	let (num, unit) = s.split_at(split);

	let value: f64 = num.trim().parse().map_err(|_| format!("invalid number: '{num}'"))?;
	if value < 0.0 {
		return Err(format!("size cannot be negative: '{s}'"));
	}

	let unit = unit.trim();
	let (is_bits, prefix) = match unit.strip_suffix('b') {
		Some(prefix) => (true, prefix),
		None => (false, unit.strip_suffix('B').unwrap_or(unit)),
	};
	if is_bits && !bits_allowed {
		return Err(format!("bit units aren't valid here, use bytes: '{s}'"));
	}

	let multiplier = parse_prefix(prefix)?;
	let bytes = value * multiplier / if is_bits { 8.0 } else { 1.0 };
	Ok(bytes.round() as u64)
}

/// Resolve an SI (`k`/`M`/`G`/`T`) or binary (`Ki`/`Mi`/`Gi`/`Ti`) prefix to a
/// multiplier. An empty prefix is 1.
fn parse_prefix(prefix: &str) -> Result<f64, String> {
	let (symbol, radix) = match prefix.strip_suffix(['i', 'I']) {
		Some(symbol) => (symbol, 1024.0_f64),
		None => (prefix, 1000.0_f64),
	};
	let exponent = match symbol {
		"" if radix == 1000.0 => return Ok(1.0),
		"k" | "K" => 1,
		"m" | "M" => 2,
		"g" | "G" => 3,
		"t" | "T" => 4,
		_ => return Err(format!("unknown unit prefix: '{prefix}'")),
	};
	Ok(radix.powi(exponent))
}

/// Format a byte count for breach messages using decimal (1000-based) units.
fn human_bytes(n: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
	let mut value = n as f64;
	let mut unit = 0;
	while value >= 1000.0 && unit < UNITS.len() - 1 {
		value /= 1000.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{n}B")
	} else {
		format!("{value:.1}{}", UNITS[unit])
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_percent_forms() {
		assert_eq!(parse_percent("75"), Ok(75.0));
		assert_eq!(parse_percent("75%"), Ok(75.0));
		assert_eq!(parse_percent(" 80.5 % "), Ok(80.5));
		assert_eq!(parse_percent("100"), Ok(100.0));
		assert!(parse_percent("abc").is_err());
		assert!(parse_percent("-5").is_err(), "negative rejected");
		assert!(parse_percent("150").is_err(), "over 100 rejected");
		// A utilization percent over 100 is a config error, not a disabled check.
		assert!("150%".parse::<MemLimit>().is_err(), "ram over 100% rejected");
		assert!("-1%".parse::<MemLimit>().is_err(), "negative ram rejected");
	}

	#[test]
	fn parse_size_units() {
		// Bytes, SI.
		assert_eq!(parse_size("500MB", false), Ok(500_000_000));
		assert_eq!(parse_size("1GB", false), Ok(1_000_000_000));
		assert_eq!(parse_size("512B", false), Ok(512));
		assert_eq!(parse_size("2G", false), Ok(2_000_000_000)); // no b/B = bytes
		// Bytes, binary.
		assert_eq!(parse_size("32GiB", false), Ok(32 * 1024 * 1024 * 1024));
		assert_eq!(parse_size("1KiB", false), Ok(1024));
		// Bits, normalized to bytes.
		assert_eq!(parse_size("4Gb", true), Ok(500_000_000));
		assert_eq!(parse_size("8b", true), Ok(1));
		// Decimals.
		assert_eq!(parse_size("1.5GB", false), Ok(1_500_000_000));
	}

	#[test]
	fn parse_size_rejects() {
		assert!(parse_size("80", false).is_err(), "bare number must be rejected");
		assert!(parse_size("10Xb", true).is_err(), "unknown prefix");
		assert!(parse_size("4Gb", false).is_err(), "bits not allowed for memory");
		assert!(parse_size("i", false).is_err(), "lone binary marker");
	}

	#[test]
	fn mem_limit_round_trips() {
		for s in ["80%", "32000000000B"] {
			let parsed: MemLimit = s.parse().unwrap();
			assert_eq!(parsed.to_string().parse::<MemLimit>().unwrap(), parsed);
		}
		assert_eq!("80%".parse::<MemLimit>().unwrap(), MemLimit::Percent(80.0));
		assert_eq!("32GB".parse::<MemLimit>().unwrap(), MemLimit::Bytes(32_000_000_000));
	}

	#[test]
	fn rate_round_trips() {
		let rate: Rate = "500MB".parse().unwrap();
		assert_eq!(rate, Rate(500_000_000));
		assert_eq!(rate.to_string().parse::<Rate>().unwrap(), rate);
		// Bits and bytes that resolve to the same rate.
		assert_eq!("4Gb".parse::<Rate>().unwrap(), "500MB".parse::<Rate>().unwrap());
	}

	#[test]
	fn evaluate_reports_breaches() {
		let cfg = HealthConfig {
			cpu: Some(75.0),
			ram: Some(MemLimit::Percent(80.0)),
			tx: Some(Rate(100_000_000)),
			..Default::default()
		};

		// All under threshold => healthy.
		let ok = Sample {
			cpu: Some(50.0),
			ram_used: Some(40),
			ram_total: Some(100),
			tx: Some(10_000_000),
			..Default::default()
		};
		assert!(evaluate(&cfg, &ok, (0.0, 0.0, 0.0), 4.0).is_empty());

		// CPU and RAM over, network under.
		let hot = Sample {
			cpu: Some(90.0),
			ram_used: Some(95),
			ram_total: Some(100),
			tx: Some(10_000_000),
			..Default::default()
		};
		let breaches = evaluate(&cfg, &hot, (0.0, 0.0, 0.0), 4.0);
		assert_eq!(breaches.len(), 2);
		assert!(breaches.iter().any(|b| b.starts_with("cpu ")));
		assert!(breaches.iter().any(|b| b.starts_with("ram ")));
	}

	#[test]
	fn evaluate_without_thresholds_is_healthy() {
		let cfg = HealthConfig::default();
		let sample = Sample {
			cpu: Some(100.0),
			..Default::default()
		};
		assert!(evaluate(&cfg, &sample, (99.0, 99.0, 99.0), 4.0).is_empty());
	}

	#[cfg(unix)]
	#[test]
	fn evaluate_loadavg_absolute() {
		let cfg = HealthConfig {
			load5: Some(LoadLimit::Absolute(1.0)),
			..Default::default()
		};
		let sample = Sample::default();
		assert!(evaluate(&cfg, &sample, (0.5, 0.5, 0.5), 4.0).is_empty());
		let breaches = evaluate(&cfg, &sample, (0.5, 2.0, 0.5), 4.0);
		assert_eq!(breaches.len(), 1);
		assert!(breaches[0].starts_with("load5 2.00 exceeds 1.00"), "{}", breaches[0]);
	}

	#[cfg(unix)]
	#[test]
	fn evaluate_loadavg_percent() {
		// 80% of 4 cores => breach when load5 > 3.2.
		let cfg = HealthConfig {
			load5: Some(LoadLimit::Percent(80.0)),
			..Default::default()
		};
		let sample = Sample::default();
		assert!(evaluate(&cfg, &sample, (0.0, 3.0, 0.0), 4.0).is_empty());
		let breaches = evaluate(&cfg, &sample, (0.0, 3.6, 0.0), 4.0);
		assert_eq!(breaches.len(), 1);
		// 3.6 / 4 cores = 90%.
		assert!(breaches[0].starts_with("load5 90.0% exceeds 80%"), "{}", breaches[0]);
	}

	#[cfg(unix)]
	#[test]
	fn load_limit_round_trips() {
		assert_eq!("80%".parse::<LoadLimit>().unwrap(), LoadLimit::Percent(80.0));
		assert_eq!("6".parse::<LoadLimit>().unwrap(), LoadLimit::Absolute(6.0));
		// A load can exceed core count, so percentages over 100 are valid.
		assert_eq!("200%".parse::<LoadLimit>().unwrap(), LoadLimit::Percent(200.0));
		for s in ["80%", "6"] {
			let parsed: LoadLimit = s.parse().unwrap();
			assert_eq!(parsed.to_string().parse::<LoadLimit>().unwrap(), parsed);
		}
		// Negatives would pin the host at 503.
		assert!("-1".parse::<LoadLimit>().is_err(), "negative load rejected");
		assert!("-5%".parse::<LoadLimit>().is_err(), "negative load percent rejected");
	}

	#[test]
	fn human_bytes_scales() {
		assert_eq!(human_bytes(512), "512B");
		assert_eq!(human_bytes(1_500_000), "1.5MB");
		assert_eq!(human_bytes(2_000_000_000), "2.0GB");
	}
}

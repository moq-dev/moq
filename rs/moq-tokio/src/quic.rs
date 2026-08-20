//! QUIC transport tuning.
//!
//! One [`Config`] (`--quic-*`) carries the per-connection knobs (stream limits,
//! GSO, timeouts) that each backend applies. They mean the same thing whichever
//! way a connection was opened, so it is spelled once and shared by
//! [`crate::connect`] and [`crate::listen`] rather than owned by either.
//!
//! The knobs that only apply when accepting (the QUIC preferred address and the
//! QUIC-LB connection-ID encoding) live on [`crate::listen::Config`] instead.
//! Not every backend honors every knob, see the field docs.

use std::path::PathBuf;
use std::time::Duration;

/// The routable server ID a QUIC-LB load balancer encodes into connection IDs.
///
/// Parsed from, and serialized as, a hex string. Its length must match the load
/// balancer's configured server-ID length.
#[serde_with::serde_as]
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerId(#[serde_as(as = "serde_with::hex::Hex")] pub(crate) Vec<u8>);

impl ServerId {
	#[allow(dead_code)]
	pub(crate) fn len(&self) -> usize {
		self.0.len()
	}
}

impl std::fmt::Debug for ServerId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_tuple("ServerId").field(&hex::encode(&self.0)).finish()
	}
}

impl std::str::FromStr for ServerId {
	type Err = hex::FromHexError;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		hex::decode(s).map(Self)
	}
}

/// The congestion control family for a QUIC connection.
///
/// This selects a family rather than a named algorithm because each backend ships a
/// different generation: BBRv1 on quinn, BBRv2 on quiche, BBRv3 on noq and iroh. A
/// `Bbr` variant would promise more than any one backend delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CongestionControl {
	/// Loss-based (CUBIC): grows until it drops packets, so the send rate sawtooths.
	/// Throughput-oriented, and the default on most stacks.
	Loss,
	/// Delay-based (BBR): tracks the measured delivery rate and RTT instead of waiting
	/// for loss, which keeps queues short and the send rate steady enough for an encoder
	/// to track.
	Delay,
}

/// Parses the same spellings the CLI and TOML accept (`loss`, `delay`), case-insensitively.
impl std::str::FromStr for CongestionControl {
	type Err = String;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		<Self as clap::ValueEnum>::from_str(s, true)
	}
}

/// Default maximum number of concurrent QUIC streams (bidi and uni) per connection.
pub(crate) const DEFAULT_MAX_STREAMS: u64 = 1024;

/// Default idle timeout before an inactive connection is dropped.
pub(crate) const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Default keep-alive ping interval.
pub(crate) const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(5);

/// The `--quic-*` transport section, applied to dialed and accepted connections alike.
#[derive(Clone, Debug, Default, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[group(id = "quic")]
#[non_exhaustive]
pub struct Config {
	/// Maximum number of concurrent QUIC streams per connection (both bidi and uni).
	/// Defaults to 1024. MoQ opens a stream per group, so busy endpoints want this high.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(id = "quic-max-streams", long = "quic-max-streams", env = "MOQ_QUIC_MAX_STREAMS")]
	pub max_streams: Option<u64>,

	/// Enable UDP generic segmentation offload (GSO).
	///
	/// GSO batches sends into one syscall for throughput, but some NICs and
	/// middleboxes mangle segmented packets. Defaults to on. The iroh backend
	/// cannot turn it off and rejects an explicit `false`.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "quic-gso",
		long = "quic-gso",
		env = "MOQ_QUIC_GSO",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub gso: Option<bool>,

	/// Idle timeout before an inactive connection is dropped. Defaults to 30s.
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	#[arg(
		id = "quic-idle-timeout",
		long = "quic-idle-timeout",
		env = "MOQ_QUIC_IDLE_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	pub idle_timeout: Option<Duration>,

	/// Keep-alive ping interval. Defaults to 5s; set `0s` to disable.
	/// Ignored by the iroh backend, which has no keep-alive knob.
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	#[arg(
		id = "quic-keep-alive",
		long = "quic-keep-alive",
		env = "MOQ_QUIC_KEEP_ALIVE",
		value_parser = humantime::parse_duration,
	)]
	pub keep_alive: Option<Duration>,

	/// Enable path MTU discovery. Defaults to off.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "quic-mtu-discovery",
		long = "quic-mtu-discovery",
		env = "MOQ_QUIC_MTU_DISCOVERY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub mtu_discovery: Option<bool>,

	/// Congestion control family. Defaults to `delay` on quinn and quiche, and to
	/// `loss` on noq and iroh, whose shared BBRv3 can panic on packet loss and take
	/// the process with it. Selecting `delay` there is for deliberate testing only.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "quic-congestion-control",
		long = "quic-congestion-control",
		env = "MOQ_QUIC_CONGESTION_CONTROL",
		value_enum
	)]
	pub congestion_control: Option<CongestionControl>,

	/// Write qlog traces into this directory, which must already exist.
	///
	/// The layout is backend-specific: quiche and noq write one file per connection,
	/// while quinn writes one file per endpoint and tags each event with the qlog
	/// `group_id` of the connection it belongs to.
	///
	/// Requires the `qlog` feature; setting it errors at init otherwise.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	#[arg(id = "quic-qlog", long = "quic-qlog", env = "MOQ_QUIC_QLOG")]
	pub qlog: Option<PathBuf>,

	/// The old role-prefixed spellings, kept parsing but hidden. Never read as
	/// settings: [`Config::deprecated`] names what replaced each one so a process
	/// can say so and stop. Not a TOML surface (config files use the canonical
	/// names).
	#[command(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,
}

/// The hidden `--client-quic-*` / `--server-quic-*` spellings (and their env
/// vars), from when the endpoint config was split by role.
#[derive(Clone, Debug, Default, clap::Args)]
#[group(id = "quic-legacy")]
pub(crate) struct Legacy {
	#[arg(
		id = "client-quic-max-streams",
		long = "client-quic-max-streams",
		alias = "client-max-streams",
		env = "MOQ_CLIENT_QUIC_MAX_STREAMS",
		hide = true
	)]
	client_max_streams: Option<u64>,

	#[arg(
		id = "server-quic-max-streams",
		long = "server-quic-max-streams",
		alias = "server-max-streams",
		env = "MOQ_SERVER_QUIC_MAX_STREAMS",
		hide = true
	)]
	server_max_streams: Option<u64>,

	#[arg(
		id = "client-quic-gso",
		long = "client-quic-gso",
		env = "MOQ_CLIENT_QUIC_GSO",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
		hide = true,
	)]
	client_gso: Option<bool>,

	#[arg(
		id = "server-quic-gso",
		long = "server-quic-gso",
		env = "MOQ_SERVER_QUIC_GSO",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
		hide = true,
	)]
	server_gso: Option<bool>,

	#[arg(
		id = "client-quic-idle-timeout",
		long = "client-quic-idle-timeout",
		env = "MOQ_CLIENT_QUIC_IDLE_TIMEOUT",
		value_parser = humantime::parse_duration,
		hide = true,
	)]
	client_idle_timeout: Option<Duration>,

	#[arg(
		id = "server-quic-idle-timeout",
		long = "server-quic-idle-timeout",
		env = "MOQ_SERVER_QUIC_IDLE_TIMEOUT",
		value_parser = humantime::parse_duration,
		hide = true,
	)]
	server_idle_timeout: Option<Duration>,

	#[arg(
		id = "client-quic-keep-alive",
		long = "client-quic-keep-alive",
		env = "MOQ_CLIENT_QUIC_KEEP_ALIVE",
		value_parser = humantime::parse_duration,
		hide = true,
	)]
	client_keep_alive: Option<Duration>,

	#[arg(
		id = "server-quic-keep-alive",
		long = "server-quic-keep-alive",
		env = "MOQ_SERVER_QUIC_KEEP_ALIVE",
		value_parser = humantime::parse_duration,
		hide = true,
	)]
	server_keep_alive: Option<Duration>,

	#[arg(
		id = "client-quic-mtu-discovery",
		long = "client-quic-mtu-discovery",
		env = "MOQ_CLIENT_QUIC_MTU_DISCOVERY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
		hide = true,
	)]
	client_mtu_discovery: Option<bool>,

	#[arg(
		id = "server-quic-mtu-discovery",
		long = "server-quic-mtu-discovery",
		env = "MOQ_SERVER_QUIC_MTU_DISCOVERY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
		hide = true,
	)]
	server_mtu_discovery: Option<bool>,

	#[arg(
		id = "client-quic-congestion-control",
		long = "client-quic-congestion-control",
		env = "MOQ_CLIENT_QUIC_CONGESTION_CONTROL",
		value_enum,
		hide = true
	)]
	client_congestion_control: Option<CongestionControl>,

	#[arg(
		id = "server-quic-congestion-control",
		long = "server-quic-congestion-control",
		env = "MOQ_SERVER_QUIC_CONGESTION_CONTROL",
		value_enum,
		hide = true
	)]
	server_congestion_control: Option<CongestionControl>,

	#[arg(
		id = "client-quic-qlog",
		long = "client-quic-qlog",
		env = "MOQ_CLIENT_QUIC_QLOG",
		hide = true
	)]
	client_qlog: Option<PathBuf>,

	#[arg(
		id = "server-quic-qlog",
		long = "server-quic-qlog",
		env = "MOQ_SERVER_QUIC_QLOG",
		hide = true
	)]
	server_qlog: Option<PathBuf>,
}

impl Legacy {
	/// The released spellings in use, each paired with what replaced it.
	///
	/// The note is the same on every line and is the point of the message: these
	/// knobs used to bound one direction, so a deployment moving to the shared
	/// section is also widening what the value applies to.
	fn deprecated(&self) -> crate::Deprecated {
		const SHARED: &str = "now applies to dialed and accepted connections alike";

		let mut found = crate::Deprecated::default();
		for (used, old, env) in [
			(
				self.client_max_streams.is_some(),
				"--client-quic-max-streams",
				"MOQ_CLIENT_QUIC_MAX_STREAMS",
			),
			(
				self.server_max_streams.is_some(),
				"--server-quic-max-streams",
				"MOQ_SERVER_QUIC_MAX_STREAMS",
			),
			(self.client_gso.is_some(), "--client-quic-gso", "MOQ_CLIENT_QUIC_GSO"),
			(self.server_gso.is_some(), "--server-quic-gso", "MOQ_SERVER_QUIC_GSO"),
			(
				self.client_idle_timeout.is_some(),
				"--client-quic-idle-timeout",
				"MOQ_CLIENT_QUIC_IDLE_TIMEOUT",
			),
			(
				self.server_idle_timeout.is_some(),
				"--server-quic-idle-timeout",
				"MOQ_SERVER_QUIC_IDLE_TIMEOUT",
			),
			(
				self.client_keep_alive.is_some(),
				"--client-quic-keep-alive",
				"MOQ_CLIENT_QUIC_KEEP_ALIVE",
			),
			(
				self.server_keep_alive.is_some(),
				"--server-quic-keep-alive",
				"MOQ_SERVER_QUIC_KEEP_ALIVE",
			),
			(
				self.client_mtu_discovery.is_some(),
				"--client-quic-mtu-discovery",
				"MOQ_CLIENT_QUIC_MTU_DISCOVERY",
			),
			(
				self.server_mtu_discovery.is_some(),
				"--server-quic-mtu-discovery",
				"MOQ_SERVER_QUIC_MTU_DISCOVERY",
			),
			(
				self.client_congestion_control.is_some(),
				"--client-quic-congestion-control",
				"MOQ_CLIENT_QUIC_CONGESTION_CONTROL",
			),
			(
				self.server_congestion_control.is_some(),
				"--server-quic-congestion-control",
				"MOQ_SERVER_QUIC_CONGESTION_CONTROL",
			),
			(self.client_qlog.is_some(), "--client-quic-qlog", "MOQ_CLIENT_QUIC_QLOG"),
			(self.server_qlog.is_some(), "--server-quic-qlog", "MOQ_SERVER_QUIC_QLOG"),
		] {
			if used {
				// `--client-quic-gso` -> `--quic-gso`, and the env var alongside it.
				let (_, knob) = old.split_once("-quic-").expect("a --<role>-quic-* spelling");
				let new = format!("--quic-{knob} / MOQ_QUIC_{}", knob.to_uppercase().replace('-', "_"));
				found.changed(old, Some(env), &new, SHARED);
			}
		}
		found
	}
}

impl Config {
	/// Every released role-prefixed spelling this config was parsed from, each
	/// paired with what replaced it.
	///
	/// A binary checks this before anything else and exits when it isn't empty. The
	/// old spellings are parsed so the process can name their replacement, not so it
	/// can honor them.
	pub fn deprecated(&self) -> crate::Deprecated {
		self.legacy.deprecated()
	}

	/// Reject knobs this build can't honor. Called when the endpoint is built.
	pub(crate) fn validate(&self) -> crate::Result<()> {
		// Erroring beats silently ignoring the flag: the operator asked for traces and
		// would otherwise go looking for files that were never going to appear.
		match self.qlog.as_ref() {
			Some(_) if cfg!(not(feature = "qlog")) => Err(crate::Error::QlogUnsupported),
			_ => Ok(()),
		}?;
		validate_idle_timeout(self.idle_timeout)
	}

	/// The per-connection knobs with defaults applied, ready to hand to a backend.
	///
	/// Reads the canonical fields only, so check [`deprecated`](Self::deprecated)
	/// first when the config came from a parser: a released spelling would show up
	/// here as the default rather than the value that was written.
	pub fn resolve(&self) -> Resolved {
		// A zero keep-alive means "disabled"; anything else (including unset) keeps
		// the connection warm, defaulting to 5s.
		let keep_alive = match self.keep_alive {
			Some(d) if d.is_zero() => None,
			Some(d) => Some(d),
			None => Some(DEFAULT_KEEP_ALIVE),
		};

		Resolved {
			max_streams: self.max_streams.unwrap_or(DEFAULT_MAX_STREAMS),
			gso: self.gso,
			idle_timeout: self.idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT),
			keep_alive,
			mtu_discovery: self.mtu_discovery.unwrap_or(false),
			congestion_control: self.congestion_control,
			qlog: self.qlog.clone(),
		}
	}
}

/// QUIC carries `max_idle_timeout` as a varint of milliseconds, so anything past this
/// can't go on the wire.
const MAX_IDLE_TIMEOUT: Duration = Duration::from_millis((1 << 62) - 1);

/// Reject an idle timeout no QUIC connection can express.
///
/// Checked here rather than in each backend because the conversion the backends do is
/// infallible-by-panic, and this config reaches them from a TOML file or a C caller.
fn validate_idle_timeout(idle_timeout: Option<Duration>) -> crate::Result<()> {
	match idle_timeout {
		Some(timeout) if timeout > MAX_IDLE_TIMEOUT => Err(crate::Error::IdleTimeoutRange),
		_ => Ok(()),
	}
}

/// Every knob at its default, which is what an untouched [`Config`] resolves to.
impl Default for Resolved {
	fn default() -> Self {
		Config::default().resolve()
	}
}

/// A resolved view of the per-connection knobs (defaults filled in), shared by
/// the dial and accept paths so backends apply them the same way regardless of role.
///
/// The backends consume it, and [`Config::resolve`] produces it. [`Resolved::default`]
/// is therefore the canonical statement of what every QUIC knob defaults to, which is
/// what a UI should show rather than repeating the numbers.
///
/// Non-exhaustive because it gains a field for every knob [`Config`] gains, so build it
/// from [`Config::resolve`] or [`Resolved::default`] rather than a struct literal.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Resolved {
	/// Max concurrent streams (bidi and uni).
	pub max_streams: u64,
	/// GSO override, or `None` to leave the backend default (on).
	pub gso: Option<bool>,
	/// Idle timeout.
	pub idle_timeout: Duration,
	/// Keep-alive interval, or `None` when disabled.
	pub keep_alive: Option<Duration>,
	/// Whether to run path MTU discovery.
	pub mtu_discovery: bool,
	/// Congestion control override, or `None` for the backend's own default. Each
	/// backend picks that default itself, since they don't all agree.
	pub congestion_control: Option<CongestionControl>,
	/// Directory to write qlog traces into, or `None` to not capture them.
	pub qlog: Option<PathBuf>,
}

impl Resolved {
	/// The directory to write qlog traces into, if any.
	///
	/// Only meaningful once [`Config::validate`] has passed; a build without the
	/// `qlog` feature never gets here with a directory set.
	#[cfg_attr(not(any(feature = "quinn", feature = "noq", feature = "quiche")), allow(dead_code))]
	pub(crate) fn qlog_dir(&self) -> Option<&std::path::Path> {
		self.qlog.as_deref()
	}

	/// Whether the config asks to turn GSO off, which not every backend can honor.
	///
	/// Only the iroh backend consults this, to reject a GSO-off request it can't
	/// satisfy; the other backends toggle GSO directly.
	#[cfg_attr(not(feature = "iroh"), allow(dead_code))]
	pub(crate) fn gso_disabled(&self) -> bool {
		self.gso == Some(false)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	/// A minimal parser so we can exercise the `--quic-*` args (and the legacy
	/// role-prefixed spellings) in isolation.
	#[derive(Parser)]
	struct Cli {
		#[command(flatten)]
		quic: Config,
	}

	fn parse(args: &[&str]) -> Config {
		let mut full = vec!["test"];
		full.extend_from_slice(args);
		Cli::parse_from(full).quic
	}

	#[test]
	fn defaults_apply_when_unset() {
		let quic = Config::default().resolve();
		assert_eq!(quic.max_streams, DEFAULT_MAX_STREAMS);
		assert_eq!(quic.idle_timeout, DEFAULT_IDLE_TIMEOUT);
		assert_eq!(quic.keep_alive, Some(DEFAULT_KEEP_ALIVE));
		assert!(!quic.mtu_discovery);
		assert_eq!(quic.gso, None);
		assert!(!quic.gso_disabled());
	}

	#[test]
	fn zero_keep_alive_disables_it() {
		let disabled = Config {
			keep_alive: Some(Duration::ZERO),
			..Default::default()
		};
		assert_eq!(disabled.resolve().keep_alive, None);

		let explicit = Config {
			keep_alive: Some(Duration::from_secs(2)),
			..Default::default()
		};
		assert_eq!(explicit.resolve().keep_alive, Some(Duration::from_secs(2)));
	}

	#[test]
	fn gso_disabled_only_on_explicit_false() {
		let off = Config {
			gso: Some(false),
			..Default::default()
		};
		assert!(off.resolve().gso_disabled());
		let on = Config {
			gso: Some(true),
			..Default::default()
		};
		assert!(!on.resolve().gso_disabled());
	}

	#[test]
	fn canonical_flags_parse() {
		let quic = parse(&[
			"--quic-max-streams",
			"5000",
			"--quic-gso=false",
			"--quic-congestion-control",
			"delay",
			"--quic-qlog",
			"/tmp/qlog",
		]);
		assert_eq!(quic.max_streams, Some(5000));
		assert_eq!(quic.gso, Some(false));
		assert_eq!(quic.congestion_control, Some(CongestionControl::Delay));
		assert_eq!(quic.qlog.as_deref(), Some(std::path::Path::new("/tmp/qlog")));
	}

	/// Every old role-prefixed spelling parses into a hidden field and is reported,
	/// never applied. The canonical knob stays unset, so a caller that skipped the
	/// check gets the default rather than a value it can't see.
	#[test]
	fn released_spellings_are_reported_not_applied() {
		let quic = parse(&["--client-quic-max-streams", "2048"]);
		assert_eq!(quic.max_streams, None);
		assert_eq!(quic.resolve().max_streams, DEFAULT_MAX_STREAMS);

		let reported = quic.deprecated().to_string();
		assert!(
			reported.contains(
				"--client-quic-max-streams / MOQ_CLIENT_QUIC_MAX_STREAMS -> --quic-max-streams / MOQ_QUIC_MAX_STREAMS"
			),
			"{reported}"
		);
		// The rename also widened what the knob covers, which the line has to say.
		assert!(reported.contains("dialed and accepted"), "{reported}");
	}

	/// The QUIC replacements are built from the flag name rather than written out,
	/// so a knob whose canonical spelling drifts would send someone to a flag that
	/// does not exist. Check every one against the parser.
	#[test]
	fn every_replacement_is_a_real_flag() {
		let canonical: std::collections::HashSet<String> =
			<Config as clap::Args>::augment_args(clap::Command::new("test"))
				.get_arguments()
				.filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
				.collect();
		let envs: std::collections::HashSet<String> = <Config as clap::Args>::augment_args(clap::Command::new("test"))
			.get_arguments()
			.filter_map(|arg| arg.get_env().map(|env| env.to_string_lossy().to_string()))
			.collect();

		for args in [
			["--client-quic-max-streams", "1"],
			["--server-quic-max-streams", "1"],
			["--client-quic-gso", "false"],
			["--server-quic-gso", "false"],
			["--client-quic-idle-timeout", "1s"],
			["--server-quic-idle-timeout", "1s"],
			["--client-quic-keep-alive", "1s"],
			["--server-quic-keep-alive", "1s"],
			["--client-quic-mtu-discovery", "false"],
			["--server-quic-mtu-discovery", "false"],
			["--client-quic-congestion-control", "loss"],
			["--server-quic-congestion-control", "loss"],
			["--client-quic-qlog", "/tmp/qlog"],
			["--server-quic-qlog", "/tmp/qlog"],
		] {
			// The booleans take `=`; the rest take a separate value.
			let spelled = format!("{}={}", args[0], args[1]);
			let reported = parse(&[&spelled]).deprecated().to_string();

			let (_, replacement) = reported.split_once(" -> ").expect("a migration line");
			let (flag, rest) = replacement.split_once(" / ").expect("flag and env");
			let env = rest.split_whitespace().next().expect("env");

			assert!(canonical.contains(flag), "{flag} is not a flag ({reported})");
			assert!(envs.contains(env), "{env} is not an env var ({reported})");
		}
	}

	/// Both role prefixes map onto the one shared knob, and each is named.
	#[test]
	fn both_role_prefixes_are_reported() {
		let reported = parse(&["--client-quic-gso=false", "--server-quic-qlog", "/tmp/qlog"])
			.deprecated()
			.to_string();
		assert!(reported.contains("--client-quic-gso"), "{reported}");
		assert!(reported.contains("--quic-gso / MOQ_QUIC_GSO"), "{reported}");
		assert!(reported.contains("--server-quic-qlog"), "{reported}");
		assert!(reported.contains("--quic-qlog / MOQ_QUIC_QLOG"), "{reported}");
	}

	/// A build that can't capture must reject the flag rather than ignore it, so an
	/// operator isn't left waiting on trace files that will never appear.
	#[test]
	fn qlog_requires_the_feature() {
		let unset = Config::default().validate();
		assert!(unset.is_ok(), "no directory configured is always fine");

		let set = Config {
			qlog: Some("/tmp/qlog".into()),
			..Default::default()
		};

		if cfg!(feature = "qlog") {
			assert!(set.validate().is_ok());
		} else {
			assert!(matches!(set.validate(), Err(crate::Error::QlogUnsupported)));
		}
	}

	/// The backends convert this duration with an infallible-by-panic `expect`, and the
	/// value arrives from a TOML file or a C caller, so validation has to catch it here.
	#[test]
	fn idle_timeout_beyond_the_varint_is_rejected() {
		let over = Config {
			idle_timeout: Some(MAX_IDLE_TIMEOUT + Duration::from_millis(1)),
			..Default::default()
		};
		assert!(matches!(over.validate(), Err(crate::Error::IdleTimeoutRange)));

		let saturated = Config {
			idle_timeout: Some(Duration::from_millis(u64::MAX)),
			..Default::default()
		};
		assert!(matches!(saturated.validate(), Err(crate::Error::IdleTimeoutRange)));

		let at_limit = Config {
			idle_timeout: Some(MAX_IDLE_TIMEOUT),
			..Default::default()
		};
		assert!(at_limit.validate().is_ok());
	}

	#[test]
	fn toml_round_trips() {
		let toml = r#"
			max_streams = 7000
			gso = false
			congestion_control = "delay"
			qlog = "/tmp/qlog"
		"#;
		let quic: Config = toml::from_str(toml).unwrap();
		assert_eq!(quic.max_streams, Some(7000));
		assert_eq!(quic.gso, Some(false));
		assert_eq!(quic.congestion_control, Some(CongestionControl::Delay));
		assert_eq!(quic.qlog.as_deref(), Some(std::path::Path::new("/tmp/qlog")));
	}
}

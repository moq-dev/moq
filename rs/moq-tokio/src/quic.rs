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

use crate::cli::Duration as CliDuration;

#[cfg(feature = "quic")]
mod backend;
#[cfg(feature = "quic")]
pub use backend::Error;
#[cfg(feature = "quic")]
pub(crate) use backend::{Client, Closer, Options, Server, accept};
#[cfg(feature = "driver")]
pub mod driver;

/// WebTransport over [`driver`], formerly the `web-transport-moq` crate.
///
/// Public only so the workspace's tests can dial a bare session; not a stable API.
#[cfg(feature = "quic")]
#[doc(hidden)]
pub mod web;

/// The routable server ID a QUIC-LB load balancer encodes into connection IDs.
///
/// Parsed from, and serialized as, a hex string. Its length must match the load
/// balancer's configured server-ID length.
#[serde_with::serde_as]
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerId(#[serde_as(as = "serde_with::hex::Hex")] pub(crate) Vec<u8>);

impl ServerId {
	#[allow(dead_code, reason = "only the QUIC backend reads it")]
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

/// Resolved QUIC-LB connection-ID encoding.
#[cfg(feature = "quic")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoadBalancer {
	/// The routable server identifier encoded into each connection ID.
	pub id: ServerId,
	/// Number of random nonce bytes appended to each connection ID.
	pub nonce: usize,
}

/// The congestion control family for a QUIC connection.
///
/// CUBIC is used for loss-based control and BBRv3 for delay-based control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, usage_rs::ValueEnum, serde::Serialize, serde::Deserialize)]
#[usage(ignore_case)]
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
	/// No congestion control and no pacing, for tests and controlled links.
	None,
}

/// Parses the same spellings the CLI and TOML accept (`loss`, `delay`, `none`), case-insensitively.
impl std::str::FromStr for CongestionControl {
	type Err = String;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		<Self as usage_rs::argv::spec::ValueEnum>::from_choice(s)
			.ok_or_else(|| format!("unknown congestion control: {s}"))
	}
}

/// Default maximum number of concurrent QUIC streams (bidi and uni) per connection.
pub const DEFAULT_MAX_STREAMS: u64 = 10_000;

/// Default connection-wide receive window, used when [`Config::receive_window`] is unset.
///
/// Bounds how much unread data and incomplete messages one peer can make us buffer across
/// all of its streams, which the backends otherwise leave unlimited. On iroh, which returns
/// credit as it reads, it bounds only the unread data. It has to be at least
/// moq-net's `transport::MIN_RECEIVE_WINDOW`, the 64 MiB announcement
/// cap plus framing, so 64 MiB would be refused; 128 MiB also holds two maximum-size frames
/// at once, and lets a relay-to-relay session sustain several Gbps at intercontinental RTTs.
pub const DEFAULT_RECEIVE_WINDOW: u64 = 128 << 20;
#[cfg(feature = "_transport")]
const _: () = assert!(DEFAULT_RECEIVE_WINDOW >= moq_net::transport::MIN_RECEIVE_WINDOW);

/// Default per-stream receive window, used when [`Config::stream_receive_window`] is unset.
///
/// Stream credit returns as bytes are read, whether or not their message is complete,
/// so this only paces one stream: enough for a single group stream to reach a few
/// hundred Mbps at a 100 ms RTT.
pub const DEFAULT_STREAM_RECEIVE_WINDOW: u64 = 4 << 20;

/// Default cap on unacknowledged outgoing data, used when [`Config::send_window`] is unset.
pub const DEFAULT_SEND_WINDOW: u64 = 16 << 20;

/// Default idle timeout before an inactive connection is dropped.
///
/// A peer that dies or drops off the network sends no close, so this is how long
/// its routes and subscriptions stay stuck. Short enough to fail over in seconds;
/// the keep-alive, derived from it, keeps a live but quiet peer from reaching it.
pub(crate) const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// The `--quic-*` transport section, applied to dialed and accepted connections alike.
#[derive(Clone, Debug, Default, usage_rs::Args, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// Maximum number of concurrent QUIC streams per connection (both bidi and uni).
	/// Defaults to 10,000; zero is refused. MoQ opens a stream per group, so busy
	/// endpoints want this high.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "quic-max-streams",
		long = "quic-max-streams",
		env = "MOQ_QUIC_MAX_STREAMS",
		setting = "quic.max_streams"
	)]
	pub max_streams: Option<u64>,

	/// Enable UDP generic segmentation offload (GSO).
	///
	/// GSO batches sends into one syscall for throughput, but some NICs and
	/// middleboxes mangle segmented packets. Defaults to on. The iroh backend
	/// cannot turn it off and rejects an explicit `false`.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "quic-gso",
		long = "quic-gso",
		env = "MOQ_QUIC_GSO",
		setting = "quic.gso",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	pub gso: Option<bool>,

	/// Idle timeout before an inactive connection is dropped. Defaults to 10s.
	///
	/// QUIC uses the smaller of both ends' timeouts, so either side can shorten it.
	/// `None` uses the default; zero disables this duration.
	#[usage(
		name = "quic-idle-timeout",
		long = "quic-idle-timeout",
		env = "MOQ_QUIC_IDLE_TIMEOUT",
		setting = "quic.idle_timeout"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub idle_timeout: Option<crate::cli::Duration>,

	/// Keep-alive ping interval; set `0s` to disable. Defaults to under a third of
	/// the idle timeout both ends settle on (3s against the 10s default), so changing
	/// the idle timeout needs no matching change here.
	///
	/// Set it for a NAT binding that lives shorter than the idle timeout. A value no
	/// shorter than the negotiated idle timeout could never keep the connection up,
	/// so it is capped at the derived interval.
	///
	/// The iroh backend applies this to its connection keep-alive. It also keeps
	/// a 5s per-path heartbeat that its builder will not raise or clear, so any
	/// interval above 5s still sends those path pings. Explicit `0s` is refused
	/// by Iroh because its builder cannot disable either heartbeat, and iroh derives
	/// the default from the local idle timeout alone.
	/// `None` uses the default; zero disables this duration.
	#[usage(
		name = "quic-keep-alive",
		long = "quic-keep-alive",
		env = "MOQ_QUIC_KEEP_ALIVE",
		setting = "quic.keep_alive"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub keep_alive: Option<crate::cli::Duration>,

	/// Enable path MTU discovery. Defaults to off.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "quic-mtu-discovery",
		long = "quic-mtu-discovery",
		env = "MOQ_QUIC_MTU_DISCOVERY",
		setting = "quic.mtu_discovery",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	pub mtu_discovery: Option<bool>,

	/// Congestion control family. Defaults to `delay`, which uses BBRv3.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "quic-congestion-control",
		long = "quic-congestion-control",
		env = "MOQ_QUIC_CONGESTION_CONTROL",
		value_enum,
		setting = "quic.congestion_control"
	)]
	pub congestion_control: Option<CongestionControl>,

	/// Connection-wide receive window, in bytes: how much data the peer may have
	/// in flight or in incomplete messages across every stream at once. Defaults to
	/// 128 MiB, and must fit the largest message a session assembles, just over 64 MiB.
	///
	/// When incomplete messages fill it, the least urgent stream is cancelled. iroh
	/// returns credit as it reads, so there it bounds only unread data.
	///
	/// Raise it when a fat, long path is idling below the link rate: the window
	/// has to cover the bandwidth-delay product or the peer stalls waiting for
	/// credit.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "quic-receive-window",
		long = "quic-receive-window",
		env = "MOQ_QUIC_RECEIVE_WINDOW",
		setting = "quic.receive_window"
	)]
	pub receive_window: Option<u64>,

	/// Per-stream receive window, in bytes. Defaults to 4 MiB.
	///
	/// Stream credit returns as bytes are read, whether or not their message is
	/// complete, so a smaller window only adds round trips to a large frame. The
	/// connection window bounds what incomplete messages hold.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "quic-stream-receive-window",
		long = "quic-stream-receive-window",
		env = "MOQ_QUIC_STREAM_RECEIVE_WINDOW",
		setting = "quic.stream_receive_window"
	)]
	pub stream_receive_window: Option<u64>,

	/// Cap on unacknowledged outgoing data, in bytes, regardless of what the peer
	/// allows. Defaults to 16 MiB.
	///
	/// This bounds the transport send buffer.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "quic-send-window",
		long = "quic-send-window",
		env = "MOQ_QUIC_SEND_WINDOW",
		setting = "quic.send_window"
	)]
	pub send_window: Option<u64>,

	/// Write qlog traces into this directory, which must already exist.
	///
	/// Each endpoint writes one file, shared by its connections.
	///
	/// Requires the `qlog` feature; setting it errors at init otherwise.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	#[usage(name = "quic-qlog", long = "quic-qlog", env = "MOQ_QUIC_QLOG", setting = "quic.qlog")]
	#[usage(value_hint = usage_rs::ValueHint::DirPath)]
	pub qlog: Option<PathBuf>,

	/// Danger: append TLS session secrets to this private file, for debugging.
	///
	/// Anyone holding this file can decrypt captured traffic. The native QUIC backend
	/// opens it at initialization; Iroh refuses this setting because its API only supports
	/// process-wide environment-based logging.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[usage(
		name = "quic-keylog",
		long = "quic-keylog",
		env = "MOQ_QUIC_KEYLOG",
		setting = "quic.keylog"
	)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath)]
	pub keylog: Option<PathBuf>,

	/// The old role-prefixed spellings, kept parsing but hidden. Never read as
	/// settings: [`Config::deprecated`] names what replaced each one so a process
	/// can say so and stop. Not a TOML surface (config files use the canonical
	/// names).
	#[usage(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,
}

/// The hidden `--client-quic-*` / `--server-quic-*` spellings (and their env
/// vars), from when the endpoint config was split by role.
#[derive(Clone, Debug, Default, usage_rs::Args)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub(crate) struct Legacy {
	#[usage(
		name = "client-quic-max-streams",
		long = "client-quic-max-streams",
		alias = "client-max-streams",
		env = "MOQ_CLIENT_QUIC_MAX_STREAMS",
		hide = true
	)]
	client_max_streams: Option<u64>,

	#[usage(
		name = "server-quic-max-streams",
		long = "server-quic-max-streams",
		alias = "server-max-streams",
		env = "MOQ_SERVER_QUIC_MAX_STREAMS",
		hide = true
	)]
	server_max_streams: Option<u64>,

	#[usage(
		name = "client-quic-gso",
		long = "client-quic-gso",
		env = "MOQ_CLIENT_QUIC_GSO",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
		hide = true,
	)]
	client_gso: Option<bool>,

	#[usage(
		name = "server-quic-gso",
		long = "server-quic-gso",
		env = "MOQ_SERVER_QUIC_GSO",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
		hide = true,
	)]
	server_gso: Option<bool>,

	#[usage(
		name = "client-quic-idle-timeout",
		long = "client-quic-idle-timeout",
		env = "MOQ_CLIENT_QUIC_IDLE_TIMEOUT",
		hide = true
	)]
	client_idle_timeout: Option<CliDuration>,

	#[usage(
		name = "server-quic-idle-timeout",
		long = "server-quic-idle-timeout",
		env = "MOQ_SERVER_QUIC_IDLE_TIMEOUT",
		hide = true
	)]
	server_idle_timeout: Option<CliDuration>,

	#[usage(
		name = "client-quic-keep-alive",
		long = "client-quic-keep-alive",
		env = "MOQ_CLIENT_QUIC_KEEP_ALIVE",
		hide = true
	)]
	client_keep_alive: Option<CliDuration>,

	#[usage(
		name = "server-quic-keep-alive",
		long = "server-quic-keep-alive",
		env = "MOQ_SERVER_QUIC_KEEP_ALIVE",
		hide = true
	)]
	server_keep_alive: Option<CliDuration>,

	#[usage(
		name = "client-quic-mtu-discovery",
		long = "client-quic-mtu-discovery",
		env = "MOQ_CLIENT_QUIC_MTU_DISCOVERY",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
		hide = true,
	)]
	client_mtu_discovery: Option<bool>,

	#[usage(
		name = "server-quic-mtu-discovery",
		long = "server-quic-mtu-discovery",
		env = "MOQ_SERVER_QUIC_MTU_DISCOVERY",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
		hide = true,
	)]
	server_mtu_discovery: Option<bool>,

	#[usage(
		name = "client-quic-congestion-control",
		long = "client-quic-congestion-control",
		env = "MOQ_CLIENT_QUIC_CONGESTION_CONTROL",
		value_enum,
		hide = true
	)]
	client_congestion_control: Option<CongestionControl>,

	#[usage(
		name = "server-quic-congestion-control",
		long = "server-quic-congestion-control",
		env = "MOQ_SERVER_QUIC_CONGESTION_CONTROL",
		value_enum,
		hide = true
	)]
	server_congestion_control: Option<CongestionControl>,

	#[usage(
		name = "client-quic-qlog",
		long = "client-quic-qlog",
		env = "MOQ_CLIENT_QUIC_QLOG",
		hide = true
	)]
	client_qlog: Option<PathBuf>,

	#[usage(
		name = "server-quic-qlog",
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
	fn deprecated(&self) -> crate::cli::Deprecated {
		const SHARED: &str = "now applies to dialed and accepted connections alike";

		let mut found = crate::cli::Deprecated::default();
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
	pub fn deprecated(&self) -> crate::cli::Deprecated {
		self.legacy.deprecated()
	}

	/// Reject knobs this build can't honor, including limits QUIC cannot
	/// advertise. Called when the endpoint is built.
	#[cfg(feature = "_transport")]
	pub fn validate(&self) -> crate::Result<()> {
		if self.keylog.as_ref().is_some_and(|path| path.as_os_str().is_empty()) {
			return Err(crate::Error::KeylogUnset);
		}
		// Erroring beats silently ignoring the flag: the operator asked for traces and
		// would otherwise go looking for files that were never going to appear.
		match self.qlog.as_ref() {
			Some(_) if cfg!(not(feature = "qlog")) => Err(crate::Error::QlogUnsupported),
			_ => Ok(()),
		}?;
		self.validate_limits()
	}

	/// Reject limits QUIC cannot advertise, through moq-quic's own checks.
	#[cfg(feature = "_transport")]
	fn validate_limits(&self) -> crate::Result<()> {
		use moq_quic::{ConfigError, LimitsError};

		// Zero credits nothing, so it would wedge the connection rather than tighten it, and
		// `Limits` would read it as unset. Refuse it here, where the message can name the knob.
		if self.max_streams == Some(0) {
			return Err(crate::Error::StreamsRange);
		}
		for (name, window) in [
			("receive_window", self.receive_window),
			("stream_receive_window", self.stream_receive_window),
			("send_window", self.send_window),
		] {
			if window == Some(0) {
				return Err(crate::Error::WindowRange(name));
			}
		}

		let resolved = self.resolve();
		let mut transport = moq_quic::transport::Config::default();
		transport.limits = resolved.limits();
		transport.idle_timeout = Some(resolved.idle_timeout);
		match transport.validate() {
			Ok(()) => {}
			Err(ConfigError::Limits(LimitsError::MaxStreams)) => return Err(crate::Error::StreamsRange),
			Err(ConfigError::Limits(LimitsError::ReceiveWindow)) => {
				return Err(crate::Error::WindowRange("receive_window"));
			}
			Err(ConfigError::Limits(LimitsError::StreamReceiveWindow)) => {
				return Err(crate::Error::WindowRange("stream_receive_window"));
			}
			Err(ConfigError::IdleTimeout) => return Err(crate::Error::IdleTimeoutRange),
			Err(err) => unreachable!("only the limits and idle timeout are set: {err}"),
		}

		// Incomplete messages hold connection credit until they are complete.
		match self.receive_window {
			Some(window) if window < moq_net::transport::MIN_RECEIVE_WINDOW => Err(
				crate::Error::ReceiveWindowTooSmall(moq_net::transport::MIN_RECEIVE_WINDOW),
			),
			_ => Ok(()),
		}
	}

	/// The per-connection knobs with defaults applied, ready to hand to a backend.
	///
	/// Reads the canonical fields only, so check [`deprecated`](Self::deprecated)
	/// first when the config came from a parser: a released spelling would show up
	/// here as the default rather than the value that was written.
	pub fn resolve(&self) -> Resolved {
		let idle_timeout = self.idle_timeout.map(Into::into).unwrap_or(DEFAULT_IDLE_TIMEOUT);
		// Unset stays unset: the backend derives it from the negotiated idle timeout.
		let keep_alive = self.keep_alive.map(Into::into);

		Resolved {
			max_streams: self.max_streams.unwrap_or(DEFAULT_MAX_STREAMS),
			gso: self.gso,
			idle_timeout,
			keep_alive,
			mtu_discovery: self.mtu_discovery.unwrap_or(false),
			congestion_control: self.congestion_control,
			receive_window: self.receive_window.unwrap_or(DEFAULT_RECEIVE_WINDOW),
			stream_receive_window: self.stream_receive_window.unwrap_or(DEFAULT_STREAM_RECEIVE_WINDOW),
			send_window: self.send_window.unwrap_or(DEFAULT_SEND_WINDOW),
			qlog: self.qlog.clone(),
			keylog: self.keylog.clone(),
		}
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
	/// Keep-alive interval, `None` to derive it from the negotiated idle timeout, or
	/// zero when disabled.
	///
	/// Unset by default, since the interval depends on the peer's idle timeout;
	/// [`keep_alive_interval`](Self::keep_alive_interval) is what it comes to
	/// against a peer that does not shorten ours.
	pub keep_alive: Option<Duration>,
	/// Whether to run path MTU discovery.
	pub mtu_discovery: bool,
	/// Congestion control override, or `None` for the default.
	pub congestion_control: Option<CongestionControl>,
	/// Connection-wide receive window in bytes, defaulting to [`DEFAULT_RECEIVE_WINDOW`].
	///
	/// The windows are resolved here rather than by each backend, whose own defaults
	/// differ (moq-quic's and iroh's connection window is unlimited), so every runtime
	/// honors the same `[quic]` section.
	pub receive_window: u64,
	/// Per-stream receive window in bytes, defaulting to [`DEFAULT_STREAM_RECEIVE_WINDOW`].
	pub stream_receive_window: u64,
	/// Cap on unacknowledged outgoing data in bytes, defaulting to [`DEFAULT_SEND_WINDOW`].
	pub send_window: u64,
	/// Directory to write qlog traces into, or `None` to not capture them.
	pub qlog: Option<PathBuf>,
	/// The explicit file to append TLS session secrets to, or `None` to disable logging.
	pub keylog: Option<PathBuf>,
}

impl Resolved {
	/// The congestion control family to install.
	///
	/// Delay-based unless the operator says otherwise: BBR keeps queues short and the
	/// send rate steady enough for a live encoder to track, which is what this stack
	/// carries.
	#[cfg_attr(
		not(any(feature = "quic", feature = "iroh")),
		allow(dead_code, reason = "only the QUIC backends use it")
	)]
	pub(crate) fn congestion(&self) -> CongestionControl {
		self.congestion_control.unwrap_or(CongestionControl::Delay)
	}

	/// The directory to write qlog traces into, if any.
	///
	/// Only meaningful once [`Config::validate`] has passed; a build without the
	/// `qlog` feature never gets here with a directory set.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub(crate) fn qlog_dir(&self) -> Option<&std::path::Path> {
		self.qlog.as_deref()
	}

	/// Whether the config asks to turn GSO off, which not every backend can honor.
	///
	/// Only the iroh backend consults this, to reject a GSO-off request it can't
	/// satisfy; the other backends toggle GSO directly.
	#[cfg_attr(not(feature = "iroh"), allow(dead_code, reason = "only the `iroh` feature uses it"))]
	pub(crate) fn gso_disabled(&self) -> bool {
		self.gso == Some(false)
	}

	/// The keep-alive interval against a peer whose idle timeout is no shorter than
	/// ours, or `None` when there is none.
	#[cfg(feature = "_transport")]
	pub fn keep_alive_interval(&self) -> Option<Duration> {
		let idle_timeout = (!self.idle_timeout.is_zero()).then_some(self.idle_timeout);
		moq_quic::transport::keep_alive_interval(self.keep_alive, idle_timeout)
	}

	/// The flow-control limits as moq-quic spells them.
	#[cfg(feature = "_transport")]
	pub(crate) fn limits(&self) -> moq_quic::Limits {
		use std::num::NonZeroU64;

		let mut limits = moq_quic::Limits::default();
		limits.max_streams = NonZeroU64::new(self.max_streams);
		limits.receive_window = NonZeroU64::new(self.receive_window);
		limits.stream_receive_window = NonZeroU64::new(self.stream_receive_window);
		limits.send_window = NonZeroU64::new(self.send_window);
		limits
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// QUIC carries `max_idle_timeout` as a varint of milliseconds, so anything past this
	/// can't go on the wire.
	#[cfg(feature = "_transport")]
	const MAX_IDLE_TIMEOUT: Duration = Duration::from_millis((1 << 62) - 1);

	/// The largest value a QUIC varint can carry.
	#[cfg(feature = "_transport")]
	const MAX_VARINT: u64 = (1 << 62) - 1;

	/// The largest stream limit QUIC can advertise, 2^60.
	#[cfg(feature = "_transport")]
	const MAX_STREAMS: u64 = 1 << 60;
	/// A minimal parser so we can exercise the `--quic-*` args (and the legacy
	/// role-prefixed spellings) in isolation.
	#[derive(usage_rs::Cli)]
	#[usage(unknown_flags = "error", args_override_self = false)]
	#[usage(settings)]
	struct Cli {
		#[usage(flatten)]
		quic: Config,
	}

	fn parse(args: &[&str]) -> Config {
		let mut argv = vec![std::ffi::OsString::from("test")];
		argv.extend(args.iter().map(std::ffi::OsString::from));
		let argv: Vec<_> = argv.iter().map(std::ffi::OsString::as_os_str).collect();
		Cli::try_parse_from(&argv).expect("valid test arguments").quic
	}

	#[test]
	fn defaults_apply_when_unset() {
		assert!(Config::default().idle_timeout.is_none());
		assert!(Config::default().keep_alive.is_none());
		let quic = Config::default().resolve();
		assert_eq!(quic.max_streams, DEFAULT_MAX_STREAMS);
		assert_eq!(quic.idle_timeout, DEFAULT_IDLE_TIMEOUT);
		assert_eq!(quic.keep_alive, None);
		assert!(!quic.mtu_discovery);
		assert_eq!(quic.gso, None);
		assert!(!quic.gso_disabled());
		assert!(quic.keylog.is_none());
	}

	#[cfg(feature = "_transport")]
	#[test]
	fn keep_alive_interval_derives_from_the_idle_timeout() {
		let interval = |idle: u64, keep_alive: Option<Duration>| {
			Config {
				idle_timeout: Some((Duration::from_secs(idle)).into()),
				keep_alive: keep_alive.map(Into::into),
				..Default::default()
			}
			.resolve()
			.keep_alive_interval()
		};
		assert_eq!(Resolved::default().keep_alive_interval(), Some(Duration::from_secs(3)));
		assert_eq!(interval(30, None), Some(Duration::from_secs(9)));
		assert_eq!(interval(0, None), None);
		assert_eq!(interval(10, Some(Duration::ZERO)), None);
		assert_eq!(interval(10, Some(Duration::from_secs(5))), Some(Duration::from_secs(5)));
		assert_eq!(
			interval(10, Some(Duration::from_secs(20))),
			Some(Duration::from_secs(3))
		);
		assert_eq!(
			interval(0, Some(Duration::from_secs(20))),
			Some(Duration::from_secs(20))
		);
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
			"--quic-keylog",
			"/tmp/keys.log",
		]);
		assert_eq!(quic.max_streams, Some(5000));
		assert_eq!(quic.gso, Some(false));
		assert_eq!(quic.congestion_control, Some(CongestionControl::Delay));
		assert_eq!(quic.qlog.as_deref(), Some(std::path::Path::new("/tmp/qlog")));
		assert_eq!(quic.keylog.as_deref(), Some(std::path::Path::new("/tmp/keys.log")));
	}

	/// `none` is spelled the same on the CLI, in TOML, and through `FromStr`.
	#[test]
	fn congestion_control_none_parses() {
		let quic = parse(&["--quic-congestion-control", "none"]);
		assert_eq!(quic.congestion_control, Some(CongestionControl::None));
		let quic: Config = toml::from_str(r#"congestion_control = "none""#).expect("parse");
		assert_eq!(quic.congestion_control, Some(CongestionControl::None));
		assert_eq!("NONE".parse(), Ok(CongestionControl::None));
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
		let canonical: std::collections::HashSet<String> = Cli::spec()
			.root
			.flags()
			.iter()
			.flat_map(|flag| flag.flag().longs.iter().map(|long| format!("--{long}")))
			.collect();
		let envs: std::collections::HashSet<String> = Cli::spec()
			.root
			.flags()
			.iter()
			.filter_map(|flag| flag.env().map(str::to_owned))
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
	#[cfg(feature = "_transport")]
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
	#[cfg(feature = "_transport")]
	#[test]
	fn idle_timeout_beyond_the_varint_is_rejected() {
		let over = Config {
			idle_timeout: Some((MAX_IDLE_TIMEOUT + Duration::from_millis(1)).into()),
			..Default::default()
		};
		assert!(matches!(over.validate(), Err(crate::Error::IdleTimeoutRange)));

		let saturated = Config {
			idle_timeout: Some((Duration::from_millis(u64::MAX)).into()),
			..Default::default()
		};
		assert!(matches!(saturated.validate(), Err(crate::Error::IdleTimeoutRange)));

		let at_limit = Config {
			idle_timeout: Some((MAX_IDLE_TIMEOUT).into()),
			..Default::default()
		};
		assert!(at_limit.validate().is_ok());
	}

	/// An unset window resolves to our default, so every backend applies the same one.
	#[test]
	fn windows_resolve_per_field() {
		let unset = Config::default().resolve();
		assert_eq!(unset.receive_window, DEFAULT_RECEIVE_WINDOW);
		assert_eq!(unset.stream_receive_window, DEFAULT_STREAM_RECEIVE_WINDOW);
		assert_eq!(unset.send_window, DEFAULT_SEND_WINDOW);

		let set = Config {
			receive_window: Some(64 << 20),
			stream_receive_window: Some(8 << 20),
			send_window: Some(32 << 20),
			..Default::default()
		}
		.resolve();
		assert_eq!(set.receive_window, 64 << 20);
		assert_eq!(set.stream_receive_window, 8 << 20);
		assert_eq!(set.send_window, 32 << 20);
	}

	/// A zero window credits nothing, so it would wedge the connection rather than
	/// tighten it. Reject it here, where the message can name the knob.
	#[cfg(feature = "_transport")]
	#[test]
	fn zero_windows_are_rejected() {
		for (name, config) in [
			(
				"receive_window",
				Config {
					receive_window: Some(0),
					..Default::default()
				},
			),
			(
				"stream_receive_window",
				Config {
					stream_receive_window: Some(0),
					..Default::default()
				},
			),
			(
				"send_window",
				Config {
					send_window: Some(0),
					..Default::default()
				},
			),
		] {
			let err = config.validate().expect_err("a zero window must be refused");
			assert!(matches!(err, crate::Error::WindowRange(knob) if knob == name), "{err}");
		}
	}

	/// Incomplete messages hold connection credit, so a connection window that can't fit
	/// the largest one could stall it forever, so `[quic]` refuses it.
	#[cfg(feature = "_transport")]
	#[test]
	fn the_receive_window_fits_the_largest_message() {
		let min = moq_net::transport::MIN_RECEIVE_WINDOW;
		let config = |window| Config {
			receive_window: Some(window),
			..Default::default()
		};
		assert!(matches!(
			config(min - 1).validate(),
			Err(crate::Error::ReceiveWindowTooSmall(got)) if got == min
		));
		assert!(config(min).validate().is_ok());
	}

	/// The receive windows go on the wire as transport parameters, so they have to fit
	/// a varint. The send window is local bookkeeping and does not.
	#[cfg(feature = "_transport")]
	#[test]
	fn receive_windows_must_fit_a_varint() {
		let over = Config {
			receive_window: Some(MAX_VARINT + 1),
			..Default::default()
		};
		assert!(matches!(
			over.validate(),
			Err(crate::Error::WindowRange("receive_window"))
		));

		let over = Config {
			stream_receive_window: Some(u64::MAX),
			..Default::default()
		};
		assert!(matches!(
			over.validate(),
			Err(crate::Error::WindowRange("stream_receive_window"))
		));

		let at_limit = Config {
			receive_window: Some(MAX_VARINT),
			stream_receive_window: Some(MAX_VARINT),
			send_window: Some(u64::MAX),
			..Default::default()
		};
		assert!(at_limit.validate().is_ok());
	}

	/// A MAX_STREAMS past 2^60 is a frame encoding error at the peer, and zero lets
	/// the peer open nothing, so `[quic]` refuses both.
	#[cfg(feature = "_transport")]
	#[test]
	fn stream_limits_must_fit_a_stream_id() {
		for max_streams in [0, MAX_STREAMS + 1] {
			let over = Config {
				max_streams: Some(max_streams),
				..Default::default()
			};
			assert!(
				matches!(over.validate(), Err(crate::Error::StreamsRange)),
				"{max_streams}"
			);
		}
		for max_streams in [1, MAX_STREAMS] {
			let at_limit = Config {
				max_streams: Some(max_streams),
				..Default::default()
			};
			assert!(at_limit.validate().is_ok(), "{max_streams}");
		}
	}
}

#[cfg(test)]
mod duration_tests {
	use super::*;
	crate::cli::duration::test_config_duration!(
		idle_timeout_set_in_code_beats_parser_default,
		Config,
		idle_timeout,
		"--quic-idle-timeout",
		|c: &Config| c.resolve().idle_timeout,
		|d| d
	);
	crate::cli::duration::test_config_duration!(
		keep_alive_set_in_code_beats_parser_default,
		Config,
		keep_alive,
		"--quic-keep-alive",
		|c: &Config| c.resolve().keep_alive,
		Some
	);
}

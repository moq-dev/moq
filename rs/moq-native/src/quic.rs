//! QUIC transport tuning, split by role.
//!
//! [`Client`] (`--client-quic-*`) and [`Server`] (`--server-quic-*`) carry the
//! per-connection knobs (stream limits, GSO, timeouts) that each backend applies.
//! [`Server`] additionally owns the knobs that only make sense when accepting
//! connections: the QUIC preferred address and the QUIC-LB connection-ID encoding.
//!
//! Each is flattened directly onto [`crate::ClientConfig`] / [`crate::ServerConfig`],
//! so the args parse straight into the config the endpoint is built from. Not
//! every backend honors every knob, see the field docs.

use std::net;
use std::time::Duration;

/// The routable server ID a QUIC-LB load balancer encodes into connection IDs.
///
/// Parsed from, and serialized as, a hex string. Its length must match the load
/// balancer's configured server-ID length.
#[serde_with::serde_as]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
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

/// Default maximum number of concurrent QUIC streams (bidi and uni) per connection.
pub(crate) const DEFAULT_MAX_STREAMS: u64 = 1024;

/// Default idle timeout before an inactive connection is dropped.
pub(crate) const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Default keep-alive ping interval.
pub(crate) const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(5);

/// The `--client-quic-*` transport section.
#[derive(Clone, Debug, Default, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Client {
	/// Maximum number of concurrent QUIC streams per connection (both bidi and uni).
	/// Defaults to 1024. MoQ opens a stream per group, so busy endpoints want this high.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "client-quic-max-streams",
		long = "client-quic-max-streams",
		alias = "client-max-streams",
		env = "MOQ_CLIENT_QUIC_MAX_STREAMS"
	)]
	pub max_streams: Option<u64>,

	/// Enable UDP generic segmentation offload (GSO).
	///
	/// GSO batches sends into one syscall for throughput, but some NICs and
	/// middleboxes mangle segmented packets. Defaults to on. Only the quinn and
	/// noq backends can turn it off; setting `false` errors at init on quiche/iroh.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "client-quic-gso",
		long = "client-quic-gso",
		env = "MOQ_CLIENT_QUIC_GSO",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub gso: Option<bool>,

	/// Idle timeout before an inactive connection is dropped. Defaults to 30s.
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	#[arg(
		id = "client-quic-idle-timeout",
		long = "client-quic-idle-timeout",
		env = "MOQ_CLIENT_QUIC_IDLE_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	pub idle_timeout: Option<Duration>,

	/// Keep-alive ping interval. Defaults to 5s; set `0s` to disable.
	/// Ignored by the quiche and iroh backends, which have no keep-alive knob.
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	#[arg(
		id = "client-quic-keep-alive",
		long = "client-quic-keep-alive",
		env = "MOQ_CLIENT_QUIC_KEEP_ALIVE",
		value_parser = humantime::parse_duration,
	)]
	pub keep_alive: Option<Duration>,

	/// Enable path MTU discovery. Defaults to off.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "client-quic-mtu-discovery",
		long = "client-quic-mtu-discovery",
		env = "MOQ_CLIENT_QUIC_MTU_DISCOVERY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub mtu_discovery: Option<bool>,
}

impl Client {
	/// The per-connection knobs with defaults applied, ready to hand to a backend.
	pub(crate) fn resolve(&self) -> Resolved {
		Resolved::new(
			self.max_streams,
			self.gso,
			self.idle_timeout,
			self.keep_alive,
			self.mtu_discovery,
		)
	}
}

/// The `--server-quic-*` transport section.
///
/// Carries the same per-connection knobs as [`Client`] plus the accept-side knobs
/// (preferred address, QUIC-LB connection IDs).
#[derive(Clone, Debug, Default, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Server {
	/// Maximum number of concurrent QUIC streams per connection (both bidi and uni).
	/// Defaults to 1024. MoQ opens a stream per group, so busy endpoints want this high.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "server-quic-max-streams",
		long = "server-quic-max-streams",
		alias = "server-max-streams",
		env = "MOQ_SERVER_QUIC_MAX_STREAMS"
	)]
	pub max_streams: Option<u64>,

	/// Enable UDP generic segmentation offload (GSO). See [`Client::gso`].
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "server-quic-gso",
		long = "server-quic-gso",
		env = "MOQ_SERVER_QUIC_GSO",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub gso: Option<bool>,

	/// Idle timeout before an inactive connection is dropped. Defaults to 30s.
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	#[arg(
		id = "server-quic-idle-timeout",
		long = "server-quic-idle-timeout",
		env = "MOQ_SERVER_QUIC_IDLE_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	pub idle_timeout: Option<Duration>,

	/// Keep-alive ping interval. Defaults to 5s; set `0s` to disable.
	/// Ignored by the quiche backend, which has no keep-alive knob.
	#[serde(default, skip_serializing_if = "Option::is_none", with = "humantime_serde::option")]
	#[arg(
		id = "server-quic-keep-alive",
		long = "server-quic-keep-alive",
		env = "MOQ_SERVER_QUIC_KEEP_ALIVE",
		value_parser = humantime::parse_duration,
	)]
	pub keep_alive: Option<Duration>,

	/// Enable path MTU discovery. Defaults to off.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "server-quic-mtu-discovery",
		long = "server-quic-mtu-discovery",
		env = "MOQ_SERVER_QUIC_MTU_DISCOVERY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub mtu_discovery: Option<bool>,

	/// IPv4 address advertised as the QUIC preferred_address.
	///
	/// Supporting clients (Chrome M131+, native Quinn) migrate to this address
	/// shortly after the handshake completes. Typical use: handshake on an
	/// anycast IP, steady-state on this host's unicast IP.
	///
	/// Honored by the Quinn and noq backends.
	#[arg(
		id = "server-preferred-v4",
		long = "server-preferred-v4",
		env = "MOQ_SERVER_PREFERRED_V4"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v4: Option<net::SocketAddrV4>,

	/// IPv6 address advertised as the QUIC preferred_address. See [`Self::preferred_v4`].
	#[arg(
		id = "server-preferred-v6",
		long = "server-preferred-v6",
		env = "MOQ_SERVER_PREFERRED_V6"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_v6: Option<net::SocketAddrV6>,

	/// Server ID to embed in connection IDs for QUIC-LB compatibility.
	/// If set, connection IDs will be derived semi-deterministically.
	#[arg(id = "server-quic-lb-id", long = "server-quic-lb-id", env = "MOQ_SERVER_QUIC_LB_ID")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quic_lb_id: Option<ServerId>,

	/// Number of random nonce bytes in QUIC-LB connection IDs.
	/// Must be at least 4, and server_id + nonce + 1 must not exceed 20.
	#[arg(
		id = "server-quic-lb-nonce",
		long = "server-quic-lb-nonce",
		requires = "server-quic-lb-id",
		env = "MOQ_SERVER_QUIC_LB_NONCE"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quic_lb_nonce: Option<usize>,
}

impl Server {
	/// The per-connection knobs with defaults applied, ready to hand to a backend.
	pub(crate) fn resolve(&self) -> Resolved {
		Resolved::new(
			self.max_streams,
			self.gso,
			self.idle_timeout,
			self.keep_alive,
			self.mtu_discovery,
		)
	}
}

/// A resolved view of the per-connection knobs (defaults filled in), shared by
/// [`Client`] and [`Server`] so backends apply them the same way regardless of role.
///
/// Internal: the backends consume it and [`crate::iroh::EndpointConfig::bind`]
/// resolves it from a [`Client`], so it never appears in the public surface.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Resolved {
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
}

impl Resolved {
	fn new(
		max_streams: Option<u64>,
		gso: Option<bool>,
		idle_timeout: Option<Duration>,
		keep_alive: Option<Duration>,
		mtu_discovery: Option<bool>,
	) -> Self {
		// A zero keep-alive means "disabled"; anything else (including unset) keeps
		// the connection warm, defaulting to 5s.
		let keep_alive = match keep_alive {
			Some(d) if d.is_zero() => None,
			Some(d) => Some(d),
			None => Some(DEFAULT_KEEP_ALIVE),
		};

		Self {
			max_streams: max_streams.unwrap_or(DEFAULT_MAX_STREAMS),
			gso,
			idle_timeout: idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT),
			keep_alive,
			mtu_discovery: mtu_discovery.unwrap_or(false),
		}
	}

	/// Whether the config asks to turn GSO off, which not every backend can honor.
	pub(crate) fn gso_disabled(&self) -> bool {
		self.gso == Some(false)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	/// Minimal parsers so we can exercise the `--client-quic-*` / `--server-quic-*`
	/// args in isolation (and together, the way relay/cli flatten both).
	#[derive(Parser)]
	struct Both {
		#[command(flatten)]
		client: Client,
		#[command(flatten)]
		server: Server,
	}

	fn parse(args: &[&str]) -> Both {
		let mut full = vec!["test"];
		full.extend_from_slice(args);
		Both::parse_from(full)
	}

	#[test]
	fn defaults_apply_when_unset() {
		let quic = Client::default().resolve();
		assert_eq!(quic.max_streams, DEFAULT_MAX_STREAMS);
		assert_eq!(quic.idle_timeout, DEFAULT_IDLE_TIMEOUT);
		assert_eq!(quic.keep_alive, Some(DEFAULT_KEEP_ALIVE));
		assert!(!quic.mtu_discovery);
		assert_eq!(quic.gso, None);
		assert!(!quic.gso_disabled());
	}

	#[test]
	fn zero_keep_alive_disables_it() {
		let disabled = Server {
			keep_alive: Some(Duration::ZERO),
			..Default::default()
		};
		assert_eq!(disabled.resolve().keep_alive, None);

		let explicit = Client {
			keep_alive: Some(Duration::from_secs(2)),
			..Default::default()
		};
		assert_eq!(explicit.resolve().keep_alive, Some(Duration::from_secs(2)));
	}

	#[test]
	fn gso_disabled_only_on_explicit_false() {
		let off = Client {
			gso: Some(false),
			..Default::default()
		};
		assert!(off.resolve().gso_disabled());
		let on = Client {
			gso: Some(true),
			..Default::default()
		};
		assert!(!on.resolve().gso_disabled());
	}

	#[test]
	fn client_and_server_flags_are_distinct() {
		let both = parse(&["--client-quic-max-streams", "5000", "--server-quic-max-streams", "9000"]);
		assert_eq!(both.client.max_streams, Some(5000));
		assert_eq!(both.server.max_streams, Some(9000));
	}

	#[test]
	fn server_only_knobs_parse() {
		let both = parse(&["--server-preferred-v4", "192.0.2.1:443", "--server-quic-lb-id", "ab"]);
		assert_eq!(both.server.preferred_v4, Some("192.0.2.1:443".parse().unwrap()));
		assert!(both.server.quic_lb_id.is_some());
		// The accept-side knobs live only on the server section.
		assert_eq!(both.client.max_streams, None);
	}

	#[test]
	fn deprecated_max_streams_aliases() {
		let both = parse(&["--client-max-streams", "2048", "--server-max-streams", "4096"]);
		assert_eq!(both.client.max_streams, Some(2048));
		assert_eq!(both.server.max_streams, Some(4096));
	}

	#[test]
	fn toml_round_trips() {
		let toml = r#"
			max_streams = 7000
			gso = false
			preferred_v4 = "192.0.2.1:443"
		"#;
		let quic: Server = toml::from_str(toml).unwrap();
		assert_eq!(quic.max_streams, Some(7000));
		assert_eq!(quic.gso, Some(false));
		assert_eq!(quic.preferred_v4, Some("192.0.2.1:443".parse().unwrap()));
	}
}

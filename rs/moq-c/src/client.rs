use std::str::FromStr;

use crate::{Error, ffi, moq_client_config};

/// One client configuration: the dial config plus the QUIC tuning it dials with.
///
/// They are separate types in `moq-tokio` (the tuning is shared by the dial and
/// accept sides of an endpoint), but a C caller configures one client, so this
/// carries both and [`parse_client`] fills whichever owns each knob.
#[derive(Clone, Default)]
pub struct Config {
	pub connect: moq_tokio::connect::Config,
	pub quic: moq_tokio::quic::Config,
	/// Dial once rather than redialing after the transport drops.
	pub once: bool,
}

/// Parse protocol version names; empty keeps the default, every supported version.
pub fn parse_versions(versions: &[String]) -> Result<Vec<moq_net::Version>, Error> {
	versions
		.iter()
		.map(|version| moq_net::Version::from_str(version).map_err(Error::InvalidConfig))
		.collect()
}

/// The QUIC knobs a client or a server config carries, each `None` when unset.
pub struct Quic<'a> {
	pub max_streams: Option<u64>,
	pub idle_timeout_us: Option<u64>,
	pub keep_alive_us: Option<u64>,
	pub gso: Option<bool>,
	pub mtu_discovery: Option<bool>,
	pub congestion_control: Option<&'a str>,
	pub qlog: Option<&'a str>,
	pub keylog: bool,
}

impl Quic<'_> {
	pub fn parse(self) -> Result<moq_tokio::quic::Config, Error> {
		let micros = |us: u64| std::time::Duration::from_micros(us).into();
		let mut quic = moq_tokio::quic::Config::default();
		quic.max_streams = self.max_streams;
		quic.idle_timeout = self.idle_timeout_us.map(micros);
		quic.keep_alive = self.keep_alive_us.map(micros);
		quic.gso = self.gso;
		quic.mtu_discovery = self.mtu_discovery;
		if let Some(family) = self.congestion_control {
			quic.congestion_control =
				Some(moq_tokio::quic::CongestionControl::from_str(family).map_err(Error::InvalidConfig)?);
		}
		quic.qlog = self.qlog.map(Into::into);
		quic.keylog = self
			.keylog
			.then(|| std::env::var_os("SSLKEYLOGFILE").unwrap_or_default().into());
		Ok(quic)
	}
}

/// What sessions opened through this library advertise in SETUP: the caller's `prefix`,
/// then moq-c, then its core.
pub fn implementation(prefix: Option<&str>) -> Result<String, Error> {
	let core = format!("moq-c/{} {}", env!("CARGO_PKG_VERSION"), moq_net::IMPLEMENTATION);
	let Some(prefix) = prefix else {
		return Ok(core);
	};
	// An empty token or stray whitespace would corrupt the list every peer splits on spaces.
	if prefix
		.split(' ')
		.any(|token| token.is_empty() || token.contains(|c: char| c.is_whitespace() || c.is_control()))
	{
		return Err(Error::InvalidConfig(format!(
			"implementation must be space-separated product tokens: {prefix:?}"
		)));
	}
	Ok(format!("{prefix} {core}"))
}

/// Build a client configuration from what C handed us.
///
/// `None` (a NULL pointer) is the defaults. Otherwise every knob is read through
/// its `has_*` flag or NULL check, so a zeroed struct also lands on the defaults
/// rather than on zero. The flag is what distinguishes "leave it alone" from an
/// explicit value: the reconnect backoff and the WebSocket fallback both default
/// to something non-zero, so a plain read would silently disable them.
///
/// Values are parsed here rather than at the point of use so a typo in a version
/// or an unparseable bind address fails the dial with a reason in `moq_error()`.
///
/// # Safety
/// - Every non-NULL pointer in `config` must be valid for its length.
pub unsafe fn parse_client(config: Option<&moq_client_config>) -> Result<Config, Error> {
	let mut out = Config::default();
	out.connect.implementation = Some(implementation(None)?);

	let Some(config) = config else {
		return Ok(out);
	};

	let prefix = unsafe { ffi::parse_str_optional(config.implementation, config.implementation_len)? };
	out.connect.implementation = Some(implementation(prefix)?);

	// Protocol
	let versions = unsafe { ffi::parse_strings(config.versions, config.versions_len)? };
	if !versions.is_empty() {
		out.connect.version = parse_versions(&versions)?;
	}
	out.once = config.once;
	out.connect.tokens = unsafe { ffi::parse_strings(config.tokens, config.tokens_len)? }
		.into_iter()
		.map(|token| {
			token
				.try_into()
				.map_err(|err: moq_net::auth::EmptyToken| Error::InvalidConfig(err.to_string()))
		})
		.collect::<Result<_, _>>()?;

	// Transport
	if let Some(bind) = unsafe { ffi::parse_str_optional(config.bind, config.bind_len)? } {
		let addr: std::net::SocketAddr = bind
			.parse()
			.map_err(|_| Error::InvalidConfig(format!("invalid bind address: {bind}")))?;
		out.connect.bind = Some(addr);
	}
	if config.has_connect_timeout {
		out.connect.timeout = Some((std::time::Duration::from_micros(config.connect_timeout_us)).into());
	}
	if config.has_failover_delay {
		out.connect.race = Some((std::time::Duration::from_micros(config.failover_delay_us)).into());
	}
	if config.has_resolution_delay {
		out.connect.resolution_delay = Some((std::time::Duration::from_micros(config.resolution_delay_us)).into());
	}
	if config.has_websocket_enabled {
		out.connect.websocket.enabled = Some(config.websocket_enabled);
	}
	if config.has_websocket_delay {
		out.connect.websocket.delay = Some((std::time::Duration::from_micros(config.websocket_delay_us)).into());
	}

	// TLS. `insecure` needs no flag: false is both "unset" and "verify".
	if config.tls_disable_verify {
		out.connect.tls.insecure = Some(true);
	}
	if config.has_tls_system_roots {
		out.connect.tls.system_roots = Some(config.tls_system_roots);
	}
	let roots = unsafe { ffi::parse_strings(config.tls_roots, config.tls_roots_len)? };
	if !roots.is_empty() {
		out.connect.tls.root = roots.into_iter().map(Into::into).collect();
	}
	let fingerprints = unsafe { ffi::parse_strings(config.tls_fingerprints, config.tls_fingerprints_len)? };
	if !fingerprints.is_empty() {
		out.connect.tls.fingerprint = fingerprints;
	}
	out.connect.tls.host_name =
		unsafe { ffi::parse_str_optional(config.tls_host_name, config.tls_host_name_len)? }.map(str::to_string);
	out.connect.tls.cert = unsafe { ffi::parse_str_optional(config.tls_cert, config.tls_cert_len)? }.map(Into::into);
	out.connect.tls.key = unsafe { ffi::parse_str_optional(config.tls_key, config.tls_key_len)? }.map(Into::into);

	// Reconnect backoff
	if config.has_backoff_initial {
		out.connect.backoff.initial = Some((std::time::Duration::from_micros(config.backoff_initial_us)).into());
	}
	if config.has_backoff_multiplier {
		out.connect.backoff.multiplier = config.backoff_multiplier;
	}
	if config.has_backoff_max {
		out.connect.backoff.max = Some((std::time::Duration::from_micros(config.backoff_max_us)).into());
	}
	if config.has_backoff_timeout {
		out.connect.backoff.timeout = Some((std::time::Duration::from_micros(config.backoff_timeout_us)).into());
	}

	// QUIC
	out.quic = unsafe {
		Quic {
			max_streams: config.has_quic_max_streams.then_some(config.quic_max_streams),
			idle_timeout_us: config.has_quic_idle_timeout.then_some(config.quic_idle_timeout_us),
			keep_alive_us: config.has_quic_keep_alive.then_some(config.quic_keep_alive_us),
			gso: config.has_quic_gso.then_some(config.quic_gso),
			mtu_discovery: config.has_quic_mtu_discovery.then_some(config.quic_mtu_discovery),
			congestion_control: ffi::parse_str_optional(config.quic_congestion_control, config.quic_congestion_control_len)?,
			qlog: ffi::parse_str_optional(config.quic_qlog, config.quic_qlog_len)?,
			keylog: config.quic_keylog,
		}
	}
	.parse()?;

	Ok(out)
}

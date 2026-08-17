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

	let Some(config) = config else {
		return Ok(out);
	};

	// Protocol
	let versions = unsafe { ffi::parse_strings(config.versions, config.versions_len)? };
	if !versions.is_empty() {
		out.connect.version = versions
			.iter()
			.map(|version| moq_net::Version::from_str(version).map_err(Error::InvalidConfig))
			.collect::<Result<_, _>>()?;
	}

	// Transport
	if let Some(backend) = unsafe { ffi::parse_str_optional(config.backend, config.backend_len)? } {
		out.connect.backend = Some(moq_tokio::QuicBackend::from_str(backend).map_err(Error::InvalidConfig)?);
	}
	if let Some(bind) = unsafe { ffi::parse_str_optional(config.bind, config.bind_len)? } {
		let addr: std::net::SocketAddr = bind
			.parse()
			.map_err(|_| Error::InvalidConfig(format!("invalid bind address: {bind}")))?;
		out.connect.bind = Some(addr);
	}
	if config.has_connect_timeout {
		out.connect.timeout = Some(std::time::Duration::from_millis(config.connect_timeout_ms));
	}
	if config.has_failover_delay {
		out.connect.race = Some(std::time::Duration::from_millis(config.failover_delay_ms));
	}
	if config.has_resolution_delay {
		out.connect.resolution_delay = Some(std::time::Duration::from_millis(config.resolution_delay_ms));
	}
	if config.has_websocket_enabled {
		out.connect.websocket.enabled = Some(config.websocket_enabled);
	}
	if config.has_websocket_delay {
		out.connect.websocket.delay = Some(std::time::Duration::from_millis(config.websocket_delay_ms));
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
		out.connect.backoff.initial = Some(std::time::Duration::from_millis(config.backoff_initial_ms));
	}
	if config.has_backoff_multiplier {
		out.connect.backoff.multiplier = Some(config.backoff_multiplier);
	}
	if config.has_backoff_max {
		out.connect.backoff.max = Some(std::time::Duration::from_millis(config.backoff_max_ms));
	}
	if config.has_backoff_timeout {
		out.connect.backoff.timeout = Some(std::time::Duration::from_millis(config.backoff_timeout_ms));
	}

	// QUIC
	if config.has_quic_max_streams {
		out.quic.max_streams = Some(config.quic_max_streams);
	}
	if config.has_quic_idle_timeout {
		out.quic.idle_timeout = Some(std::time::Duration::from_millis(config.quic_idle_timeout_ms));
	}
	if config.has_quic_keep_alive {
		out.quic.keep_alive = Some(std::time::Duration::from_millis(config.quic_keep_alive_ms));
	}
	if config.has_quic_gso {
		out.quic.gso = Some(config.quic_gso);
	}
	if config.has_quic_mtu_discovery {
		out.quic.mtu_discovery = Some(config.quic_mtu_discovery);
	}
	if let Some(family) =
		unsafe { ffi::parse_str_optional(config.quic_congestion_control, config.quic_congestion_control_len)? }
	{
		out.quic.congestion_control =
			Some(moq_tokio::quic::CongestionControl::from_str(family).map_err(Error::InvalidConfig)?);
	}
	out.quic.qlog = unsafe { ffi::parse_str_optional(config.quic_qlog, config.quic_qlog_len)? }.map(Into::into);

	Ok(out)
}

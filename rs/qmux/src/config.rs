use std::time::Duration;

use moq_quic::VarInt;
use moq_quic::mux;

use crate::Version;

/// The default and minimum `max_record_size`: 16 KiB less a 2-byte Size field
const DEFAULT_MAX_RECORD_SIZE: u64 = 16382;

/// How the application-level protocol is determined for a session.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum Protocol {
	/// No application protocol.
	#[default]
	None,

	/// Advertise these protocols and negotiate one in-band, via the QMux
	/// `application_protocols` transport parameter (preference order).
	///
	/// For transports without ALPN (TCP, Unix sockets). Both peers must opt in:
	/// receiving the parameter while *not* in this mode is a protocol error, and so is
	/// agreeing on none ([`Error::TransportParameter`](crate::Error::TransportParameter)).
	/// The agreed protocol is resolved by the time
	/// [`Session::connect`](crate::Session::connect) / [`accept`](crate::Session::accept)
	/// returns.
	Negotiate(Vec<String>),

	/// Already negotiated out of band (TLS / WebSocket ALPN). Reported as-is;
	/// the `application_protocols` parameter is never sent, and receiving it is
	/// a protocol error.
	Negotiated(String),
}

/// Configuration for a QMux session.
///
/// Construct with [`Config::new`] and set the public fields you
/// need. The transport entry points take this same config.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// Wire format version.
	pub version: Version,
	/// How the application protocol is determined. See [`Protocol`].
	pub protocol: Protocol,

	/// Max concurrent bidirectional streams the peer can open.
	pub max_streams_bidi: u64,
	/// Max concurrent unidirectional streams the peer can open.
	pub max_streams_uni: u64,
	/// Connection-level receive window in bytes.
	pub receive_window: u64,
	/// Per-stream receive window in bytes, for every stream.
	pub stream_receive_window: u64,

	/// Close the session after this long without hearing from the peer, keeping it alive with
	/// `QX_PING` meanwhile, or `None` to never time out. Zero is refused. The smaller of the two peers' values
	/// applies. Only the record-framed drafts (QMux01+) negotiate it.
	pub idle_timeout: Option<Duration>,
	/// Maximum QMux Record size in bytes (draft-01+), at least the default. Default: 16382.
	pub max_record_size: u64,

	/// Largest DATAGRAM *frame* (RFC 9221: frame type + length + payload) we
	/// advertise willingness to receive, in bytes; `None` disables datagrams.
	///
	/// This is a frame size, not a payload size: the usable payload is this value less the
	/// per-frame overhead. A datagram must fit within a single record, so this is capped at
	/// [`max_record_size`](Config::max_record_size). Datagrams are a record-framed-draft
	/// feature (QMux01+), so this is ignored on QMux00. Default: 16382 bytes.
	pub max_datagram_frame_size: Option<u64>,

	/// How long [`Session::connect`](crate::Session::connect) /
	/// [`accept`](crate::Session::accept) waits for the peer's transport parameters, or
	/// `None` to wait indefinitely. Bounds the handshake so a peer that completes the
	/// transport connection but never sends its parameters can't hang establishment
	/// forever. Default: 10s.
	pub handshake_timeout: Option<Duration>,

	/// How often to send a `QX_PING` request purely to measure round-trip time, or `None`
	/// to not measure it. Zero is refused.
	///
	/// Separate from the keep-alive ping, which is activity-gated and so never fires while
	/// an application is actively sending. Only the record-framed drafts (QMux01+) carry
	/// `QX_PING`. Default: 1s.
	pub rtt_interval: Option<Duration>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			version: Version::QMux02,
			protocol: Protocol::None,
			max_streams_bidi: 100,
			max_streams_uni: 100,
			receive_window: 1 << 20,
			stream_receive_window: 256 << 10,
			idle_timeout: Some(Duration::from_secs(30)),
			max_record_size: DEFAULT_MAX_RECORD_SIZE,
			// Fill a full record by default; the record layer bounds the size.
			max_datagram_frame_size: Some(DEFAULT_MAX_RECORD_SIZE),
			handshake_timeout: Some(Duration::from_secs(10)),
			rtt_interval: Some(Duration::from_secs(1)),
		}
	}
}

impl Config {
	/// Create a config with default flow control values and no application
	/// protocol. Set [`Config::protocol`] to negotiate one.
	pub fn new(version: Version) -> Self {
		Self {
			version,
			..Default::default()
		}
	}

	/// This config with the out-of-band negotiation's outcome applied.
	#[cfg(any(feature = "tls", feature = "ws"))]
	pub(crate) fn with_negotiated(self, version: Version, protocol: Option<String>) -> Self {
		Self {
			version,
			protocol: match protocol {
				Some(name) => Protocol::Negotiated(name),
				None => Protocol::None,
			},
			..self
		}
	}

	/// The protocol reported before any in-band negotiation.
	pub(crate) fn negotiated_protocol(&self) -> Option<String> {
		match &self.protocol {
			Protocol::Negotiated(name) => Some(name.clone()),
			Protocol::None | Protocol::Negotiate(_) => None,
		}
	}

	/// The connection's settings, refusing a value the wire cannot carry, the peer would
	/// refuse, or that never lets a timer rest
	pub(crate) fn to_mux(&self) -> Result<mux::Config, crate::Error> {
		fn varint(value: u64) -> Result<VarInt, crate::Error> {
			VarInt::from_u64(value).map_err(|_| crate::Error::InvalidConfig("value exceeds a QUIC varint"))
		}
		if self.max_datagram_frame_size == Some(0) {
			return Err(crate::Error::InvalidConfig("zero max_datagram_frame_size; use None"));
		}
		// `None` is how to disable these; zero would fire on every poll
		if self.idle_timeout == Some(Duration::ZERO) {
			return Err(crate::Error::InvalidConfig("zero idle_timeout"));
		}
		if self.rtt_interval == Some(Duration::ZERO) {
			return Err(crate::Error::InvalidConfig("zero rtt_interval"));
		}

		// QUIC's stream count ceiling (RFC 9000 §4.6), which peers enforce on the parameter
		const MAX_STREAM_COUNT: u64 = 1 << 60;
		if self.max_streams_bidi > MAX_STREAM_COUNT || self.max_streams_uni > MAX_STREAM_COUNT {
			return Err(crate::Error::InvalidConfig("stream count above 2^60"));
		}

		let mut config = mux::Config::default();
		config.version = self.version;
		config.max_concurrent_bidi_streams = varint(self.max_streams_bidi)?;
		config.max_concurrent_uni_streams = varint(self.max_streams_uni)?;
		config.receive_window = varint(self.receive_window)?;
		config.stream_receive_window = varint(self.stream_receive_window)?;
		config.max_idle_timeout = self.idle_timeout;
		if self.version.uses_records() && self.max_record_size < DEFAULT_MAX_RECORD_SIZE {
			return Err(crate::Error::InvalidConfig("max_record_size below 16382"));
		}
		config.max_record_size = varint(self.max_record_size)?;
		config.max_datagram_frame_size = self.max_datagram_frame_size.map(varint).transpose()?;
		config.handshake_timeout = self.handshake_timeout;
		config.rtt_interval = self.rtt_interval;
		// Only advertise protocols when negotiating in-band; TLS/WS already chose one via
		// ALPN and must not send this parameter.
		config.protocols = match &self.protocol {
			Protocol::Negotiate(list) => Some(list.clone()),
			Protocol::None | Protocol::Negotiated(_) => None,
		};
		Ok(config)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn record_size_below_the_minimum_is_refused() {
		for &version in Version::ALL {
			let config = Config {
				max_record_size: DEFAULT_MAX_RECORD_SIZE - 1,
				..Config::new(version)
			};
			assert_eq!(config.to_mux().is_err(), version.uses_records(), "{version:?}");
		}
	}

	#[test]
	fn datagrams_are_disabled_explicitly() {
		let disabled = Config {
			max_datagram_frame_size: None,
			..Config::default()
		};
		assert!(disabled.to_mux().unwrap().max_datagram_frame_size.is_none());
		let zero = Config {
			max_datagram_frame_size: Some(0),
			..Config::default()
		};
		assert!(matches!(zero.to_mux(), Err(crate::Error::InvalidConfig(_))));
	}

	#[test]
	fn zero_timers_are_refused() {
		let idle = Config {
			idle_timeout: Some(Duration::ZERO),
			..Config::default()
		};
		assert!(matches!(idle.to_mux(), Err(crate::Error::InvalidConfig(_))));
		let rtt = Config {
			rtt_interval: Some(Duration::ZERO),
			..Config::default()
		};
		assert!(matches!(rtt.to_mux(), Err(crate::Error::InvalidConfig(_))));
	}

	#[test]
	fn stream_counts_above_quics_ceiling_are_refused() {
		let at = Config {
			max_streams_bidi: 1 << 60,
			max_streams_uni: 1 << 60,
			..Config::default()
		};
		assert!(at.to_mux().is_ok());
		for (bidi, uni) in [((1 << 60) + 1, 0), (0, (1 << 60) + 1)] {
			let over = Config {
				max_streams_bidi: bidi,
				max_streams_uni: uni,
				..Config::default()
			};
			assert!(matches!(over.to_mux(), Err(crate::Error::InvalidConfig(_))));
		}
	}
}

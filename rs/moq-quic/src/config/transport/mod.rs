//! Parameters governing the core QUIC state machine, the same for either role

#[cfg(test)]
use rand::RngExt;
use std::{fmt, num::NonZeroU64, sync::Arc};

use crate::{
	ConfigError, Duration, INITIAL_MTU, Limits, MAX_UDP_PAYLOAD, VarInt, congestion, connection::qlog::QlogSink,
};

pub mod ack_frequency;
pub mod datagram;
pub mod mtu;
#[cfg(feature = "qlog")]
pub mod qlog;

/// The expected round trip time the default windows are tuned for, in milliseconds
const EXPECTED_RTT: u32 = 100;
/// The per-stream bandwidth the default windows are tuned for, in bytes per second
const MAX_STREAM_BANDWIDTH: u32 = 12500 * 1000;
/// The per-stream window that avoids pipeline stalls at [`MAX_STREAM_BANDWIDTH`]
const STREAM_RWND: u32 = MAX_STREAM_BANDWIDTH / 1000 * EXPECTED_RTT;

/// The stream limit [`Limits::max_streams`] defaults to
const MAX_STREAMS: u64 = 100;

/// Parameters governing the core QUIC state machine
///
/// Default values should be suitable for most internet applications.
///
/// In some cases, performance or resource requirements can be improved by tuning these values to
/// suit a particular application and/or network connection. In particular, data window sizes can be
/// tuned for a particular expected round trip time, link capacity, and memory availability. Tuning
/// for higher bandwidths and latencies increases worst-case memory consumption, but does not impair
/// performance at lower bandwidths and latencies. The default configuration is tuned for a 100Mbps
/// link with a 100ms round trip time.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// Flow-control windows and stream counts
	///
	/// Each `None` takes its default: 100 streams of each direction, a 1.25 MB window per
	/// stream, an unlimited connection window, and 10 MB of unacknowledged outgoing data.
	///
	/// Worst-case memory use is directly proportional to `max_streams * stream_receive_window`,
	/// with an upper bound proportional to `receive_window`.
	pub limits: Limits,

	/// Maximum duration of inactivity to accept before timing out the connection, or `None` for
	/// no timeout
	///
	/// The true idle timeout is the minimum of this and the peer's own. Must fit a QUIC varint in
	/// milliseconds. Defaults to 30 seconds, as RFC 9308 section 3.2 recommends.
	///
	/// **WARNING**: If a peer or its network path malfunctions or acts maliciously, an infinite
	/// idle timeout can result in permanently hung futures!
	pub idle_timeout: Option<Duration>,

	/// Maximum duration of inactivity to accept before abandoning a handshake
	///
	/// Used in place of [`idle_timeout`](Self::idle_timeout) until the handshake completes, so a
	/// short idle timeout meant to detect a dead peer quickly does not also cut a handshake off
	/// after a single probe. It applies whether the idle timeout is shorter, longer, or disabled,
	/// and whatever idle timeout the peer advertises. Defaults to 10 seconds, as msquic's
	/// `HandshakeIdleTimeoutMs`.
	pub handshake_timeout: Duration,

	/// Period of inactivity before sending a keep-alive packet, or `None` (the default) to send
	/// none
	///
	/// Keep-alive packets prevent an inactive but otherwise healthy connection from timing out.
	/// Only one side of any given connection needs keep-alive enabled for the connection to be
	/// preserved. Must be set lower than the idle timeout of both peers to be effective.
	pub keep_alive: Option<Duration>,

	/// Whether to implement fair queuing for send streams having the same priority, on by default
	///
	/// When enabled, connections schedule data from outgoing streams having the same priority in a
	/// round-robin fashion. When disabled, streams are scheduled in the order they are written to.
	/// Higher priority streams always take precedence over lower priority streams.
	///
	/// Disabling fairness can reduce fragmentation and protocol overhead for workloads that use
	/// many small streams.
	pub send_fairness: bool,

	/// Maximum reordering in packet number space before FACK style loss detection considers a
	/// packet lost. Should not be less than 3, the default, per RFC 5681.
	pub packet_threshold: u32,

	/// Maximum reordering in time space before time based loss detection considers a packet lost,
	/// as a factor of RTT. Defaults to 9/8.
	pub time_threshold: f32,

	/// The RTT used before an RTT sample is taken, 333ms by default as RFC 9002 recommends
	pub initial_rtt: Duration,

	/// Number of consecutive PTOs after which the network is considered to be experiencing
	/// persistent congestion, 3 by default
	pub persistent_congestion_threshold: u32,

	/// The maximum UDP payload size to use before running MTU discovery
	///
	/// Must be at least 1200, the default, which is known to be safe for typical internet
	/// applications. Larger values are more efficient, but increase the risk of packet loss due to
	/// exceeding the network path's IP MTU. If the provided value is higher than what the network
	/// path actually supports, packet loss will eventually trigger black hole detection and bring
	/// it down to [`min_mtu`](Self::min_mtu).
	pub initial_mtu: u16,

	/// The maximum UDP payload size guaranteed to be supported by the network
	///
	/// Must be at least 1200, the default. A larger [`initial_mtu`](Self::initial_mtu) starts
	/// there instead.
	///
	/// Client Initial datagrams and ack-eliciting server Initial datagrams are padded to at least
	/// this size, subject to the peer's maximum UDP payload size.
	///
	/// Real-world MTUs can vary according to ISP, VPN, and properties of intermediate network links
	/// outside of either endpoint's control. If this value exceeds what the network path supports,
	/// connections may fail to be established because Initial datagrams cannot be delivered. Prefer
	/// [`initial_mtu`](Self::initial_mtu) together with [`mtu`](Self::mtu) to set a maximum UDP
	/// payload size that robustly adapts to the network.
	pub min_mtu: u16,

	/// Pad UDP datagrams carrying application data to current maximum UDP payload size, off by
	/// default
	///
	/// UDP datagrams containing loss probes are exempt from padding. Enabling this helps mitigate
	/// traffic analysis by network observers, but it increases bandwidth usage.
	pub pad_to_mtu: bool,

	/// How MTU discovery runs, or `None` to stay at [`initial_mtu`](Self::initial_mtu)
	///
	/// Enabled by default.
	pub mtu: Option<mtu::Config>,

	/// The acknowledgement frequency to request of the peer, or `None` (the default) to leave the
	/// peer's own
	///
	/// Ignored if the peer does not support the acknowledgement frequency QUIC extension. Even when
	/// `None`, the local side still supports the extension.
	pub ack_frequency: Option<ack_frequency::Config>,

	/// How to construct each connection's congestion controller, BBRv3 by default
	pub congestion: Arc<dyn congestion::ControllerFactory + Send + Sync>,

	/// An outbound rate limit for each connection, in bytes per second, or `None` (the default)
	/// for none
	///
	/// The congestion controller's pacer enforces it, so a controller that does not pace, such as
	/// `Unlimited`, ignores it.
	pub max_outgoing_bytes_per_second: Option<u64>,

	/// How unreliable datagrams are buffered
	pub datagram: datagram::Config,

	/// Maximum quantity of out-of-order crypto layer data to buffer, 16 KiB by default
	pub crypto_buffer_size: usize,

	/// Whether the implementation is permitted to set the spin bit, on by default
	///
	/// This allows passive observers to easily judge the round trip time of a connection, which can
	/// be useful for network administration but sacrifices a small amount of privacy.
	pub allow_spin: bool,

	/// Whether to use "Generic Segmentation Offload" to accelerate transmits, when supported by
	/// the environment, on by default
	///
	/// GSO dramatically reduces CPU consumption when sending large numbers of packets with the same
	/// headers, such as when transmitting bulk data on a connection. However, it is not supported
	/// by all network interface drivers or packet inspection tools. The UDP driver may
	/// disable GSO automatically when unavailable, but this can lead to spurious packet loss at
	/// startup, temporarily degrading performance.
	pub segmentation_offload: bool,

	/// Where to write qlog traces, or `None` (the default) to write none
	#[cfg(feature = "qlog")]
	pub qlog: Option<qlog::Stream>,

	/// Whether to advertise support for receiving RESET_STREAM_AT
	pub(crate) reset_stream_at: bool,

	/// Whether to force every packet number to be used
	///
	/// By default, packet numbers are occasionally skipped to ensure peers aren't ACKing packets
	/// before they see them.
	#[cfg(test)]
	pub(crate) deterministic_packet_numbers: bool,
}

impl Config {
	/// Refuse what a peer would reject or the state machine cannot honor
	///
	/// Endpoints call this on every config they are handed, so call it only to fail earlier.
	pub fn validate(&self) -> Result<(), ConfigError> {
		self.limits.validate()?;
		if self
			.idle_timeout
			.is_some_and(|timeout| VarInt::try_from(timeout.as_millis()).is_err())
		{
			return Err(ConfigError::IdleTimeout);
		}
		let mtu = INITIAL_MTU..=MAX_UDP_PAYLOAD;
		let discovery = self.mtu.as_ref().map(|mtu| mtu.upper_bound);
		if !mtu.contains(&self.initial_mtu)
			|| !mtu.contains(&self.min_mtu)
			|| discovery.is_some_and(|bound| !mtu.contains(&bound))
		{
			return Err(ConfigError::Mtu);
		}
		Ok(())
	}

	/// The stream limit for each direction
	pub(crate) fn max_streams(&self) -> VarInt {
		varint(self.limits.max_streams, MAX_STREAMS)
	}

	/// The receive window for each stream
	pub(crate) fn stream_receive_window(&self) -> VarInt {
		varint(self.limits.stream_receive_window, STREAM_RWND.into())
	}

	/// The receive window across every stream
	pub(crate) fn receive_window(&self) -> VarInt {
		varint(self.limits.receive_window, VarInt::MAX.into_inner())
	}

	/// The unacknowledged outgoing data to buffer across every stream
	pub(crate) fn send_window(&self) -> u64 {
		self.limits
			.send_window
			.map_or(8 * u64::from(STREAM_RWND), NonZeroU64::get)
	}

	/// The idle timeout as the transport parameter carries it, in milliseconds
	pub(crate) fn max_idle_timeout(&self) -> Option<VarInt> {
		let timeout = self.idle_timeout?;
		Some(VarInt::try_from(timeout.as_millis()).expect("validated"))
	}

	pub(crate) fn get_initial_mtu(&self) -> u16 {
		self.initial_mtu.max(self.min_mtu)
	}

	pub(crate) fn qlog_sink(&self) -> QlogSink {
		#[cfg(feature = "qlog")]
		return self.qlog.clone().into();
		#[cfg(not(feature = "qlog"))]
		QlogSink::default()
	}
}

/// A limit on the wire, or its default when unset
fn varint(limit: Option<NonZeroU64>, default: u64) -> VarInt {
	VarInt::from_u64(limit.map_or(default, NonZeroU64::get)).expect("validated")
}

impl Default for Config {
	fn default() -> Self {
		Self {
			limits: Limits::default(),
			idle_timeout: Some(Duration::from_secs(30)),
			handshake_timeout: Duration::from_secs(10),
			keep_alive: None,
			send_fairness: true,

			packet_threshold: 3,
			time_threshold: 9.0 / 8.0,
			initial_rtt: Duration::from_millis(333), // per spec, intentionally distinct from EXPECTED_RTT
			persistent_congestion_threshold: 3,
			initial_mtu: INITIAL_MTU,
			min_mtu: INITIAL_MTU,
			pad_to_mtu: false,
			mtu: Some(mtu::Config::default()),
			ack_frequency: None,

			congestion: Arc::new({
				let config = congestion::Bbr3Config::default();
				#[cfg(test)]
				let config = {
					let mut config = config;
					config.probe_rng_seed(crate::entropy::rng().random());
					config
				};
				config
			}),
			max_outgoing_bytes_per_second: None,

			datagram: datagram::Config {
				receive_buffer: Some(STREAM_RWND as usize),
				send_buffer: 1024 * 1024,
			},
			crypto_buffer_size: 16 * 1024,
			allow_spin: true,
			segmentation_offload: true,
			#[cfg(feature = "qlog")]
			qlog: None,

			reset_stream_at: true,
			#[cfg(test)]
			deterministic_packet_numbers: false,
		}
	}
}

impl fmt::Debug for Config {
	fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
		let Self {
			limits,
			idle_timeout,
			handshake_timeout,
			keep_alive,
			send_fairness,
			packet_threshold,
			time_threshold,
			initial_rtt,
			persistent_congestion_threshold,
			initial_mtu,
			min_mtu,
			pad_to_mtu,
			mtu,
			ack_frequency,
			congestion: _,
			max_outgoing_bytes_per_second,
			datagram,
			crypto_buffer_size,
			allow_spin,
			segmentation_offload,
			#[cfg(feature = "qlog")]
			qlog,
			reset_stream_at,
			#[cfg(test)]
				deterministic_packet_numbers: _,
		} = self;
		let mut s = fmt.debug_struct("Config");
		s.field("limits", limits)
			.field("idle_timeout", idle_timeout)
			.field("handshake_timeout", handshake_timeout)
			.field("keep_alive", keep_alive)
			.field("send_fairness", send_fairness)
			.field("packet_threshold", packet_threshold)
			.field("time_threshold", time_threshold)
			.field("initial_rtt", initial_rtt)
			.field("persistent_congestion_threshold", persistent_congestion_threshold)
			.field("initial_mtu", initial_mtu)
			.field("min_mtu", min_mtu)
			.field("pad_to_mtu", pad_to_mtu)
			.field("mtu", mtu)
			.field("ack_frequency", ack_frequency)
			// congestion not debug
			.field("max_outgoing_bytes_per_second", max_outgoing_bytes_per_second)
			.field("datagram", datagram)
			.field("crypto_buffer_size", crypto_buffer_size)
			.field("allow_spin", allow_spin)
			.field("segmentation_offload", segmentation_offload)
			.field("reset_stream_at", reset_stream_at);
		#[cfg(feature = "qlog")]
		s.field("qlog", &qlog.is_some());
		s.finish_non_exhaustive()
	}
}

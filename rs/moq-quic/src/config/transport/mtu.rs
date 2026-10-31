//! Path MTU discovery

use crate::Duration;

/// Parameters governing MTU discovery.
///
/// # The why of MTU discovery
///
/// By design, QUIC ensures during the handshake that the network path between the client and the
/// server is able to transmit unfragmented UDP packets with a body of 1200 bytes. In other words,
/// once the connection is established, we know that the network path's maximum transmission unit
/// (MTU) is of at least 1200 bytes (plus IP and UDP headers). Because of this, a QUIC endpoint can
/// split outgoing data in packets of 1200 bytes, with confidence that the network will be able to
/// deliver them (if the endpoint were to send bigger packets, they could prove too big and end up
/// being dropped).
///
/// There is, however, a significant overhead associated to sending a packet. If the same
/// information can be sent in fewer packets, that results in higher throughput. The amount of
/// packets that need to be sent is inversely proportional to the MTU: the higher the MTU, the
/// bigger the packets that can be sent, and the fewer packets that are needed to transmit a given
/// amount of bytes.
///
/// Most networks have an MTU higher than 1200. Through MTU discovery, endpoints can detect the
/// path's MTU and, if it turns out to be higher, start sending bigger packets.
///
/// # MTU discovery internals
///
/// The transport implements MTU discovery through DPLPMTUD (Datagram Packetization Layer Path MTU
/// Discovery), described in [section 14.3 of RFC
/// 9000](https://www.rfc-editor.org/rfc/rfc9000.html#section-14.3). This method consists of sending
/// QUIC packets padded to a particular size (called PMTU probes), and waiting to see if the remote
/// peer responds with an ACK. If an ACK is received, that means the probe arrived at the remote
/// peer, which in turn means that the network path's MTU is of at least the packet's size. If the
/// probe is lost, it is sent another 2 times before concluding that the MTU is lower than the
/// packet's size.
///
/// MTU discovery runs on a schedule (e.g. every 600 seconds) specified through
/// [`interval`](Self::interval). The first run happens right after the handshake, and
/// subsequent discoveries are scheduled to run when the interval has elapsed, starting from the
/// last time when MTU discovery completed.
///
/// Since the search space for MTUs is quite big (the smallest possible MTU is 1200, and the highest
/// is 65527), the transport performs a binary search to keep the number of probes as low as possible. The
/// lower bound of the search is equal to [`initial_mtu`](super::Config::initial_mtu) in the
/// initial MTU discovery run, and is equal to the currently discovered MTU in subsequent runs. The
/// upper bound is determined by the minimum of [`upper_bound`](Self::upper_bound) and the
/// `max_udp_payload_size` transport parameter received from the peer during the handshake.
///
/// # Black hole detection
///
/// If, at some point, the network path no longer accepts packets of the detected size, packet loss
/// will eventually trigger black hole detection and reset the detected MTU to 1200. In that case,
/// MTU discovery will be triggered after [`black_hole_cooldown`](Self::black_hole_cooldown) (ignoring the
/// timer that was set based on [`interval`](Self::interval)).
///
/// # Interaction between peers
///
/// There is no guarantee that the MTU on the path between A and B is the same as the MTU of the
/// path between B and A. Therefore, each peer in the connection needs to run MTU discovery
/// independently in order to discover the path's MTU.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// The time to wait after completing MTU discovery before starting a new MTU discovery run
	///
	/// Defaults to 600 seconds, as recommended by [RFC 8899](https://www.rfc-editor.org/rfc/rfc8899).
	pub interval: Duration,

	/// The upper bound to the max UDP payload size that MTU discovery will search for
	///
	/// Defaults to 1452, to stay within Ethernet's MTU when using IPv4 and IPv6. Must be between
	/// 1200 and 65527, the maximum permitted UDP payload on IPv6.
	///
	/// It is safe to use an arbitrarily high upper bound, regardless of the network path's MTU. The
	/// only drawback is that MTU discovery might take more time to finish.
	pub upper_bound: u16,

	/// The minimum MTU change to stop the MTU discovery phase, 20 by default
	pub minimum_change: u16,

	/// How long MTU discovery waits after a black hole was detected before running again, one
	/// minute by default
	///
	/// Black hole detection can be spuriously triggered in case of congestion, so it makes sense to
	/// try MTU discovery again after a short period of time.
	pub black_hole_cooldown: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			interval: Duration::from_secs(600),
			upper_bound: 1452,
			black_hole_cooldown: Duration::from_secs(60),
			minimum_change: 20,
		}
	}
}

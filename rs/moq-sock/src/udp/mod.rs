//! Uniform interface to send and receive UDP packets with advanced features useful for QUIC
//!
//! A hard fork of [quinn-udp](https://github.com/quinn-rs/quinn); all credit goes to the quinn
//! developers. See `rs/moq-quic/README.md` for the upstream commit and carried changes.
//!
//! This module exposes kernel UDP stack features available on most modern systems which are required
//! for an efficient and conformant QUIC implementation. As of this writing, these are not available
//! in std or major async runtimes, and their niche character and complexity are a barrier to adding
//! them.
//!
//! Exposed features include:
//!
//! - Segmentation offload for bulk send and receive operations, reducing CPU load.
//! - Reporting the exact destination address of received packets and specifying explicit source
//!   addresses for sent packets, allowing responses to be sent from the address that the peer
//!   expects when there are multiple possibilities. This is common when bound to a wildcard address
//!   in IPv6 due to [RFC 8981] temporary addresses.
//! - [Explicit Congestion Notification], which is required by QUIC to prevent packet loss and reduce
//!   latency on congested links when supported by the network path.
//! - Disabled IP-layer fragmentation, which allows the true physical MTU to be detected and reduces
//!   risk of QUIC packet loss.
//!
//! Some features are unavailable in some environments. This can be due to an outdated operating
//! system or drivers. Some operating systems may not implement desired features at all, or may not
//! yet be supported by the module. When support is unavailable, functionality will gracefully
//! degrade.
//!
//! [RFC 8981]: https://www.rfc-editor.org/rfc/rfc8981.html
//! [Explicit Congestion Notification]: https://www.rfc-editor.org/rfc/rfc3168.html
#![warn(unreachable_pub)]
#![warn(clippy::use_self)]

use core::time::Duration;
#[cfg(any(unix, target_os = "wasi"))]
use std::os::fd::AsFd;
#[cfg(windows)]
use std::os::windows::io::AsSocket;
use std::{
	io,
	net::{IpAddr, Ipv6Addr, SocketAddr},
};
#[cfg(target_os = "wasi")]
use std::{
	mem::ManuallyDrop,
	net::UdpSocket,
	os::fd::{AsRawFd, FromRawFd},
};
#[cfg(windows)]
use windows_sys::Win32::Networking::WinSock;

#[cfg(apple_fast)]
mod apple_fast;

#[cfg(any(unix, windows))]
mod cmsg;

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

#[cfg(target_os = "wasi")]
#[path = "wasi.rs"]
mod imp;

#[allow(
	unused_imports,
	reason = "moq-sock always logs through `tracing`, so quinn-udp's `log` and no-op backends are not carried"
)]
mod log {
	pub(crate) use tracing::{debug, error, info, trace, warn};
}

pub use imp::UdpSocketState;

/// Number of UDP packets to send/receive at a time
pub const BATCH_SIZE: usize = imp::BATCH_SIZE;

/// Metadata for a single buffer filled with bytes received from the network
///
/// This associated buffer can contain one or more datagrams, see [`stride`].
///
/// [`stride`]: RecvMeta::stride
#[derive(Debug, Copy, Clone)]
#[non_exhaustive]
pub struct RecvMeta {
	/// The source address of the datagram(s) contained in the buffer
	pub addr: SocketAddr,
	/// The number of bytes the associated buffer has
	pub len: usize,
	/// The size of a single datagram in the associated buffer
	///
	/// When GRO (Generic Receive Offload) is used this indicates the size of a single
	/// datagram inside the buffer. If the buffer is larger, that is if [`len`] is greater
	/// then this value, then the individual datagrams contained have their boundaries at
	/// `stride` increments from the start. The last datagram could be smaller than
	/// `stride`.
	///
	/// [`len`]: RecvMeta::len
	pub stride: usize,
	/// The Explicit Congestion Notification bits for the datagram(s) in the buffer
	pub ecn: Option<EcnCodepoint>,
	/// The destination IP address which was encoded in this datagram
	///
	/// Populated on platforms: Windows, Linux, Android (API level > 25),
	/// FreeBSD, OpenBSD, NetBSD, macOS, and iOS.
	pub dst_ip: Option<IpAddr>,
	/// The interface index of the interface on which the datagram was received
	pub interface_index: Option<u32>,
	/// Kernel receive timestamp as Unix epoch
	///
	/// Populated on platforms: Linux, Android.
	pub timestamp: Option<Duration>,
}

impl Default for RecvMeta {
	/// Constructs a value with arbitrary fields, intended to be overwritten
	fn default() -> Self {
		Self {
			addr: SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0),
			len: 0,
			stride: 0,
			ecn: None,
			dst_ip: None,
			interface_index: None,
			timestamp: None,
		}
	}
}

/// An outgoing packet
#[derive(Debug, Clone)]
pub struct Transmit<'a> {
	/// The socket this datagram should be sent to
	pub destination: SocketAddr,
	/// Explicit congestion notification bits to set on the packet
	pub ecn: Option<EcnCodepoint>,
	/// Contents of the datagram
	pub contents: &'a [u8],
	/// The segment size if this transmission contains multiple datagrams.
	/// This is `None` if the transmit only contains a single datagram
	pub segment_size: Option<usize>,
	/// Optional source IP address for the datagram
	pub src_ip: Option<IpAddr>,
}

impl Transmit<'_> {
	/// Computes the effective segment-size of the packet.
	///
	/// Some (older) network drivers don't like being told to do GSO even if
	/// there is effectively only a single segment.
	/// (i.e. `segment_size == contents.len()`)
	/// Additionally, a `segment_size` that is greater than the content also
	/// means there is effectively only a single segment.
	/// This case is actually quite common when splitting up a prepared GSO batch
	/// again after GSO has been disabled because the last datagram in a GSO
	/// batch is allowed to be smaller than the segment size.
	#[cfg_attr(
		apple_fast,
		allow(dead_code, reason = "used by prepare_msg, which is unused when apple_fast")
	)]
	fn effective_segment_size(&self) -> Option<usize> {
		match self.segment_size? {
			size if size >= self.contents.len() => None,
			size => Some(size),
		}
	}
}

/// Asynchronous transport-layer errors reported by the operating system
///
/// On Linux and Android these are delivered via the socket error queue
/// (`MSG_ERRQUEUE`) and originate from ICMP messages.
///
/// These errors are out-of-band and do not correspond to a received packet.
#[derive(Debug, Copy, Clone)]
#[non_exhaustive]
pub struct TransportError {
	/// Address associated with the error
	///
	/// This is the remote peer or an intermediate network device that triggered the error.
	/// Returns `None` if the kernel cannot determine the source (e.g. `AF_UNSPEC`).
	pub addr: Option<SocketAddr>,
	/// Transport-layer error details
	pub payload: TransportErrorPayload,
	/// The raw error code from the underlying operating system
	pub raw_errno: i32,
}

impl TransportError {
	/// Returns the recommended MTU for packet-too-big errors
	pub fn mtu(&self) -> Option<u32> {
		match self.payload {
			TransportErrorPayload::TooBig { mtu } => Some(mtu),
			_ => None,
		}
	}
}

/// Transport-layer error details reported by the kernel
#[derive(Debug, Copy, Clone)]
#[non_exhaustive]
pub enum TransportErrorPayload {
	/// Destination host or port is unreachable
	Unreachable,
	/// Packet exceeds path MTU
	TooBig {
		/// Recommended Maximum Transmission Unit
		mtu: u32,
	},
	/// Other transport-layer or kernel-reported error
	Other,
}

/// Returns true if the given I/O error corresponds to a message size error
///
/// Useful, for example, after invoking `std::io::Error::last_os_error()`
/// to check if the last OS error was a message size error
/// (EMSGSIZE on Unix, WSAEMSGSIZE on Windows).
///
/// Note: EMSGSIZE's value is not standardized across OSes (90 on Linux,
/// 40 on macOS/iOS/BSD; on Windows, `io::Error::raw_os_error()` returns
/// the Winsock error WSAEMSGSIZE (10040) via `GetLastError()`, which is
/// distinct from the MSVCRT `errno.h` EMSGSIZE value of 115, which is
/// never actually populated by socket operations).
pub fn is_msg_size_err(err: &io::Error) -> bool {
	#[cfg(unix)]
	{
		err.raw_os_error() == Some(libc::EMSGSIZE)
	}
	#[cfg(windows)]
	{
		err.raw_os_error() == Some(WinSock::WSAEMSGSIZE)
	}
	#[cfg(not(any(unix, windows)))]
	{
		false
	}
}

/// Returns true if the given send error means the local stack has no way to reach the destination
///
/// Most send errors are transient, so higher layers treat them as packet loss. These are not:
/// there is no route to the address (ENETUNREACH, EHOSTUNREACH) or the interface is down
/// (ENETDOWN), and the Windows `WSAE*` equivalents. Retransmitting cannot fix them, so a
/// connection attempt can give up on that address at once, for example to dial the other address
/// family.
///
/// They are only local for an unconnected socket with transport errors left disabled, as
/// `UdpSocketState` sockets are by default. A `connect`ed socket, or one with `IP_RECVERR`, can
/// report a received ICMP error here instead, which an off-path attacker can forge.
pub fn is_unreachable_err(err: &io::Error) -> bool {
	matches!(
		err.kind(),
		io::ErrorKind::NetworkUnreachable | io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkDown
	)
}

/// A borrowed UDP socket
///
/// On Unix, constructible via `From<T: AsFd>`. On Windows, constructible via `From<T:
/// AsSocket>`.
// Wrapper around socket2 to avoid making it a public dependency and incurring stability risk
pub struct UdpSockRef<'a>(socket2::SockRef<'a>);

impl UdpSockRef<'_> {
	/// Configures the socket for non-blocking or blocking operation.
	pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
		#[cfg(not(target_os = "wasi"))]
		self.0.set_nonblocking(nonblocking)?;

		#[cfg(target_os = "wasi")]
		{
			// socket2 uses `fcntl`, which WASI rejects; `std` uses `ioctl(FIONBIO)`.
			// Safety: `self` outlives the borrow, and `ManuallyDrop` prevents a second close.
			let borrowed = ManuallyDrop::new(unsafe { UdpSocket::from_raw_fd(self.0.as_raw_fd()) });
			borrowed.set_nonblocking(nonblocking)?;
		}

		Ok(())
	}
}

#[cfg(any(unix, target_os = "wasi"))]
impl<'s, S> From<&'s S> for UdpSockRef<'s>
where
	S: AsFd,
{
	fn from(socket: &'s S) -> Self {
		Self(socket.into())
	}
}

#[cfg(windows)]
impl<'s, S> From<&'s S> for UdpSockRef<'s>
where
	S: AsSocket,
{
	fn from(socket: &'s S) -> Self {
		Self(socket.into())
	}
}

/// Explicit congestion notification codepoint
#[repr(u8)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum EcnCodepoint {
	/// The ECT(0) codepoint, indicating that an endpoint is ECN-capable
	Ect0 = 0b10,
	/// The ECT(1) codepoint, indicating that an endpoint is ECN-capable
	Ect1 = 0b01,
	/// The CE codepoint, signalling that congestion was experienced
	Ce = 0b11,
}

impl EcnCodepoint {
	/// Create new object from the given bits
	pub fn from_bits(x: u8) -> Option<Self> {
		use EcnCodepoint::*;
		Some(match x & 0b11 {
			0b10 => Ect0,
			0b01 => Ect1,
			0b11 => Ce,
			_ => {
				return None;
			}
		})
	}
}

#[cfg(test)]
mod tests {
	use std::net::Ipv4Addr;

	use super::*;

	#[cfg(unix)]
	#[test]
	fn unreachable_errors_are_only_the_routing_failures() {
		for code in [libc::ENETUNREACH, libc::EHOSTUNREACH, libc::ENETDOWN] {
			let err = io::Error::from_raw_os_error(code);
			assert!(is_unreachable_err(&err), "{err}");
		}
		// Loss, readiness, MTU probes, the GSO fallback, and ICMP noise all stay retryable.
		for code in [
			libc::ENOBUFS,
			libc::EAGAIN,
			libc::EMSGSIZE,
			libc::EINVAL,
			libc::EIO,
			libc::ECONNREFUSED,
		] {
			let err = io::Error::from_raw_os_error(code);
			assert!(!is_unreachable_err(&err), "{err}");
		}
	}

	#[test]
	fn effective_segment_size() {
		assert_eq!(
			make_transmit(&[0u8; 10], Some(15)).effective_segment_size(),
			None,
			"segment_size > content_len should yield no effective segment_size"
		);
		assert_eq!(
			make_transmit(&[0u8; 10], Some(10)).effective_segment_size(),
			None,
			"segment_size == content_len should yield no effective segment_size"
		);
		assert_eq!(
			make_transmit(&[0u8; 10], None).effective_segment_size(),
			None,
			"no segment_size should yield no effective segment_size"
		);
		assert_eq!(
			make_transmit(&[0u8; 10], Some(5)).effective_segment_size(),
			Some(5),
			"segment_size < content_len should yield effective segment_size"
		);
	}

	fn make_transmit(contents: &[u8], segment_size: Option<usize>) -> Transmit<'_> {
		Transmit {
			destination: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 1)),
			ecn: None,
			contents,
			segment_size,
			src_ip: None,
		}
	}
}

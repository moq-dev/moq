//! Statistics read from the connection underneath QMux.
//!
//! QMux runs over a reliable byte stream and has no congestion controller of its
//! own, but the transport it sits on usually does. Where that transport is TCP the
//! kernel already tracks a smoothed RTT and (on Linux) a delivery-rate estimate,
//! so the figures are available immediately and cost no extra traffic. This is
//! strictly better than QMux's own [`QX_PING`](crate::Config::rtt_interval)
//! measurement, which needs a round trip before it can report anything.

use std::sync::Arc;
use std::time::Duration;

/// Live statistics for the connection underneath a QMux session.
///
/// Implementations read through to the socket on each call rather than caching, so
/// a caller polling this sees the transport's current view. Every metric is
/// optional: `None` means this transport doesn't track it.
pub trait SocketStats: Send + Sync + 'static {
	/// Smoothed round-trip time, if the transport tracks one.
	fn rtt(&self) -> Option<Duration> {
		None
	}

	/// Estimated delivery rate in **bits per second**, if the transport tracks one.
	fn estimated_send_rate(&self) -> Option<u64> {
		None
	}

	/// Hold the transport's own unsent queue to about one congestion window, returning that
	/// bound in bytes, or `None` to leave the transport's buffering alone.
	///
	/// A session calls this when it starts and again whenever a write finds the transport
	/// full, since the window moves, and hands over at most the bound per batch. What the
	/// transport holds unsent can no longer be dropped when its stream resets or expires, so
	/// a smaller queue sheds stale media sooner; less than one window would leave the link
	/// idle while the session refills it.
	fn limit_unsent(&self) -> Option<usize> {
		None
	}
}

/// A shared handle to a transport's statistics.
pub type SharedSocketStats = Arc<dyn SocketStats>;

/// Windowed maximum of delivery-rate samples, like BBR's bandwidth filter.
///
/// A live publisher is usually application-limited: it has one frame to send every
/// few milliseconds, so most kernel samples measure the sender's own rate rather
/// than the path's. Such a sample may only raise the estimate. A sample that is not
/// app-limited is a real measurement and replaces older maxima once they leave the
/// window. With no new usable sample the last estimate stands: an app-limited flow
/// is no evidence that the path got slower. A zero rate is no sample at all.
#[derive(Debug, Default)]
#[cfg(any(target_os = "linux", test))]
pub(crate) struct RateFilter {
	/// Strictly decreasing rates from front to back; the front is the maximum.
	samples: std::collections::VecDeque<(kio::clock::Instant, u64)>,
}

#[cfg(any(target_os = "linux", test))]
impl RateFilter {
	/// About ten round trips on a WAN path, as in BBR.
	pub(crate) const WINDOW: Duration = Duration::from_secs(10);

	/// Fold in one sample and return the current estimate, if there is one yet.
	pub(crate) fn update(&mut self, now: kio::clock::Instant, rate: u64, app_limited: bool) -> Option<u64> {
		let expired = |at: &kio::clock::Instant| now.saturating_duration_since(*at) > Self::WINDOW;
		let usable = rate > 0 && (!app_limited || self.samples.front().is_none_or(|(_, max)| rate >= *max));
		// A rejected sample changes nothing, not even expiry: it is no evidence the
		// path got slower, so only a usable sample may retire an old maximum.
		if usable {
			while self.samples.front().is_some_and(|(at, _)| expired(at)) {
				self.samples.pop_front();
			}
			while self.samples.back().is_some_and(|(_, r)| *r <= rate) {
				self.samples.pop_back();
			}
			self.samples.push_back((now, rate));
		}
		self.samples.front().map(|(_, max)| *max)
	}
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::TcpStats;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use unix::unsent_bound;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
	use super::*;
	use std::io;
	use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
	use std::sync::atomic::{AtomicU32, Ordering};

	/// Kernel TCP statistics for a socket, via `TCP_INFO` (Linux) or
	/// `TCP_CONNECTION_INFO` (macOS).
	///
	/// Holds its own duplicate of the file descriptor, so it stays readable for as
	/// long as the session needs it no matter what the transport does with the
	/// original. Construct it from the accepted socket and hand it to the transport
	/// with `with_socket_stats`.
	///
	/// Reports nothing for a non-TCP socket (a Unix socket, say): the `getsockopt`
	/// simply fails and every metric reads `None`.
	#[derive(Debug)]
	pub struct TcpStats {
		fd: OwnedFd,
		/// Delivery-rate samples, filtered so app-limited ones cannot lower the estimate.
		#[cfg(target_os = "linux")]
		rate: std::sync::Mutex<RateFilter>,
		/// The congestion window the unsent bound was last set from, so an unchanged window
		/// costs no `setsockopt`.
		/// Zero until the first, since a live socket's window never is.
		window: AtomicU32,
	}

	impl TcpStats {
		/// Duplicate `socket`'s descriptor and read statistics from the copy.
		pub fn new<S: AsFd>(socket: &S) -> io::Result<Self> {
			Ok(Self::from_fd(socket.as_fd().try_clone_to_owned()?))
		}

		/// Take ownership of an already-duplicated descriptor.
		///
		/// Use this when the socket itself has been moved somewhere unreachable (a
		/// type-erased HTTP upgrade, for instance) and only the descriptor was kept.
		pub fn from_fd(fd: OwnedFd) -> Self {
			Self {
				fd,
				#[cfg(target_os = "linux")]
				rate: Default::default(),
				window: AtomicU32::new(0),
			}
		}

		fn borrow(&self) -> BorrowedFd<'_> {
			self.fd.as_fd()
		}
	}

	#[cfg(target_os = "linux")]
	mod sys {
		use super::*;

		/// Mask for `tcpi_delivery_rate_app_limited`, the first bitfield of its byte,
		/// which C allocates from the low bit on little-endian targets and from the
		/// high bit on big-endian ones.
		#[cfg(target_endian = "little")]
		pub const APP_LIMITED_MASK: u8 = 1 << 0;
		#[cfg(target_endian = "big")]
		pub const APP_LIMITED_MASK: u8 = 1 << 7;

		/// `struct tcp_info` from `linux/tcp.h`.
		///
		/// Mirrored field-for-field rather than hand-computing offsets, so `repr(C)`
		/// alignment places each member where the kernel put it. The kernel reports
		/// how much it actually filled in, which is less than this on older
		/// releases, so every read checks its field against that length.
		#[repr(C)]
		#[derive(Default, Clone, Copy)]
		pub struct TcpInfo {
			pub state: u8,
			pub ca_state: u8,
			pub retransmits: u8,
			pub probes: u8,
			pub backoff: u8,
			pub options: u8,
			pub wscale: u8,
			/// `tcpi_delivery_rate_app_limited:1` then `tcpi_fastopen_client_fail:2`.
			pub app_limited: u8,

			pub rto: u32,
			pub ato: u32,
			pub snd_mss: u32,
			pub rcv_mss: u32,

			pub unacked: u32,
			pub sacked: u32,
			pub lost: u32,
			pub retrans: u32,
			pub fackets: u32,

			pub last_data_sent: u32,
			pub last_ack_sent: u32,
			pub last_data_recv: u32,
			pub last_ack_recv: u32,

			pub pmtu: u32,
			pub rcv_ssthresh: u32,
			/// Smoothed RTT in microseconds.
			pub rtt: u32,
			pub rttvar: u32,
			pub snd_ssthresh: u32,
			pub snd_cwnd: u32,
			pub advmss: u32,
			pub reordering: u32,

			pub rcv_rtt: u32,
			pub rcv_space: u32,
			pub total_retrans: u32,

			pub pacing_rate: u64,
			pub max_pacing_rate: u64,
			pub bytes_acked: u64,
			pub bytes_received: u64,
			pub segs_out: u32,
			pub segs_in: u32,
			pub notsent_bytes: u32,
			pub min_rtt: u32,
			pub data_segs_out: u32,
			pub data_segs_in: u32,
			/// Delivery-rate estimate in bytes per second (Linux 4.9+).
			pub delivery_rate: u64,
		}

		const IPPROTO_TCP: libc::c_int = 6;
		const TCP_INFO: libc::c_int = 11;

		/// Read `tcp_info`, returning it alongside the number of bytes the kernel
		/// filled in. `None` if the socket isn't TCP.
		pub fn tcp_info(fd: BorrowedFd<'_>) -> Option<(TcpInfo, usize)> {
			let mut info = TcpInfo::default();
			let mut len = std::mem::size_of::<TcpInfo>() as libc::socklen_t;

			// SAFETY: `info` is a live, correctly sized allocation and `len` says how
			// large it is; the kernel writes at most that much and updates `len` to
			// what it wrote.
			let rc = unsafe {
				libc::getsockopt(
					fd.as_raw_fd(),
					IPPROTO_TCP,
					TCP_INFO,
					(&mut info as *mut TcpInfo).cast(),
					&mut len,
				)
			};

			(rc == 0).then_some((info, len as usize))
		}

		/// Whether the kernel filled in the field at `offset` of size `size`.
		pub fn has_field(len: usize, offset: usize, size: usize) -> bool {
			len >= offset + size
		}

		/// Linux stops a write once the unsent bytes reach `TCP_NOTSENT_LOWAT`, so the mark is
		/// the window itself.
		pub const UNSENT_BOUND: (libc::c_int, libc::c_int) = (libc::IPPROTO_TCP, libc::TCP_NOTSENT_LOWAT);

		pub fn unsent_bound(window: u64) -> u64 {
			window
		}

		/// The congestion window in bytes: segments times the sending MSS.
		pub fn congestion_window(fd: BorrowedFd<'_>) -> Option<u64> {
			let (info, len) = tcp_info(fd)?;
			let filled = |offset| has_field(len, offset, size_of::<u32>());
			(filled(std::mem::offset_of!(TcpInfo, snd_cwnd)) && filled(std::mem::offset_of!(TcpInfo, snd_mss)))
				.then(|| u64::from(info.snd_cwnd) * u64::from(info.snd_mss))
		}
	}

	#[cfg(target_os = "macos")]
	mod sys {
		use super::*;

		/// `struct tcp_connection_info` from macOS `netinet/tcp.h`.
		#[repr(C)]
		#[derive(Default, Clone, Copy)]
		pub struct TcpInfo {
			pub state: u8,
			pub snd_wscale: u8,
			pub rcv_wscale: u8,
			_pad1: u8,
			pub options: u32,
			pub flags: u32,
			pub rto: u32,
			pub maxseg: u32,
			pub snd_ssthresh: u32,
			pub snd_cwnd: u32,
			pub snd_wnd: u32,
			pub snd_sbbytes: u32,
			pub rcv_wnd: u32,
			/// Most recent RTT sample, in milliseconds.
			pub rttcur: u32,
			/// Smoothed RTT in milliseconds.
			pub srtt: u32,
			pub rttvar: u32,
		}

		const IPPROTO_TCP: libc::c_int = 6;
		const TCP_CONNECTION_INFO: libc::c_int = 0x106;

		pub fn tcp_info(fd: BorrowedFd<'_>) -> Option<(TcpInfo, usize)> {
			let mut info = TcpInfo::default();
			let mut len = std::mem::size_of::<TcpInfo>() as libc::socklen_t;

			// SAFETY: as for the Linux arm above.
			let rc = unsafe {
				libc::getsockopt(
					fd.as_raw_fd(),
					IPPROTO_TCP,
					TCP_CONNECTION_INFO,
					(&mut info as *mut TcpInfo).cast(),
					&mut len,
				)
			};

			(rc == 0).then_some((info, len as usize))
		}

		pub fn has_field(len: usize, offset: usize, size: usize) -> bool {
			len >= offset + size
		}

		/// macOS takes `TCP_NOTSENT_LOWAT` only as a readiness hint, and a write fills the
		/// whole send buffer regardless, so bound the buffer instead. It holds what is in
		/// flight, up to a window, as well as what is unsent: two windows leave one unsent.
		pub const UNSENT_BOUND: (libc::c_int, libc::c_int) = (libc::SOL_SOCKET, libc::SO_SNDBUF);

		pub fn unsent_bound(window: u64) -> u64 {
			window.saturating_mul(2)
		}

		/// The congestion window, which macOS already counts in bytes.
		pub fn congestion_window(fd: BorrowedFd<'_>) -> Option<u64> {
			let (info, len) = tcp_info(fd)?;
			let offset = std::mem::offset_of!(TcpInfo, snd_cwnd);
			has_field(len, offset, size_of::<u32>()).then_some(u64::from(info.snd_cwnd))
		}
	}

	/// Bound the bytes the kernel takes beyond what it has sent to about `window`.
	fn set_unsent_bound(fd: BorrowedFd<'_>, window: u32) -> io::Result<()> {
		let bound = sys::unsent_bound(u64::from(window));
		let value = libc::c_int::try_from(bound).unwrap_or(libc::c_int::MAX);
		let (level, name) = sys::UNSENT_BOUND;
		// SAFETY: `value` is a live `c_int` and the length says so; the kernel only reads it.
		let rc = unsafe {
			libc::setsockopt(
				fd.as_raw_fd(),
				level,
				name,
				(&value as *const libc::c_int).cast(),
				size_of::<libc::c_int>() as libc::socklen_t,
			)
		};
		match rc {
			0 => Ok(()),
			_ => Err(io::Error::last_os_error()),
		}
	}

	impl SocketStats for TcpStats {
		fn rtt(&self) -> Option<Duration> {
			let (info, len) = sys::tcp_info(self.borrow())?;

			// A successful `getsockopt` on a connected socket is a measurement, so
			// report it even at zero. macOS counts in whole milliseconds, and a
			// loopback round trip genuinely rounds to zero there.
			#[cfg(target_os = "linux")]
			{
				let offset = std::mem::offset_of!(sys::TcpInfo, rtt);
				sys::has_field(len, offset, size_of::<u32>()).then(|| Duration::from_micros(info.rtt as u64))
			}

			#[cfg(target_os = "macos")]
			{
				let offset = std::mem::offset_of!(sys::TcpInfo, srtt);
				sys::has_field(len, offset, size_of::<u32>()).then(|| Duration::from_millis(info.srtt as u64))
			}
		}

		fn estimated_send_rate(&self) -> Option<u64> {
			// Only Linux measures a delivery rate. macOS exposes a congestion window
			// but no rate, and cwnd/rtt is a poor stand-in, so report nothing there
			// rather than a figure callers would mistake for a measurement.
			#[cfg(target_os = "linux")]
			{
				let (info, len) = sys::tcp_info(self.borrow())?;
				let offset = std::mem::offset_of!(sys::TcpInfo, delivery_rate);
				if !sys::has_field(len, offset, size_of::<u64>()) {
					return None;
				}
				let app_limited = info.app_limited & sys::APP_LIMITED_MASK != 0;
				// The kernel reports bytes per second; the trait wants bits.
				let rate = info.delivery_rate.saturating_mul(8);
				let mut filter = self.rate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
				filter.update(kio::clock::now(), rate, app_limited)
			}

			#[cfg(not(target_os = "linux"))]
			{
				None
			}
		}

		fn limit_unsent(&self) -> Option<usize> {
			let window = sys::congestion_window(self.borrow()).filter(|&window| window > 0)?;
			let window = u32::try_from(window).unwrap_or(u32::MAX);
			// Linux wakes writers on a changed mark, so a raised one frees a parked write at once.
			if self.window.load(Ordering::Relaxed) != window {
				// Only a socket already torn down refuses it, and its session is ending anyway.
				set_unsent_bound(self.borrow(), window).ok()?;
				self.window.store(window, Ordering::Relaxed);
			}
			usize::try_from(window).ok()
		}
	}

	/// The option bounding `socket`'s unsent bytes, and the value its congestion window
	/// calls for.
	#[cfg(test)]
	pub(crate) fn unsent_bound<S: AsFd>(socket: &S) -> (u64, u64) {
		let fd = socket.as_fd();
		let mut value: libc::c_int = 0;
		let mut len = size_of::<libc::c_int>() as libc::socklen_t;
		// SAFETY: `value` is a live `c_int` and `len` says so.
		let (level, name) = sys::UNSENT_BOUND;
		let rc = unsafe {
			libc::getsockopt(
				fd.as_raw_fd(),
				level,
				name,
				(&mut value as *mut libc::c_int).cast(),
				&mut len,
			)
		};
		assert_eq!(rc, 0, "{}", io::Error::last_os_error());
		let window = sys::congestion_window(fd).expect("a TCP socket");
		// Linux's unbounded default reads back as -1.
		(u64::try_from(value).unwrap_or(u64::MAX), sys::unsent_bound(window))
	}

	/// Pin the field offsets against the kernel's struct.
	///
	/// The struct is mirrored field-for-field so `repr(C)` computes the offsets,
	/// which means a typo in the middle silently shifts everything after it and we
	/// would read a plausible-looking wrong number rather than fail.
	#[cfg(test)]
	mod tests {
		/// Offsets from `linux/tcp.h`.
		#[cfg(target_os = "linux")]
		#[test]
		fn linux_tcp_info_layout() {
			assert_eq!(std::mem::offset_of!(super::sys::TcpInfo, rtt), 68);
			assert_eq!(std::mem::offset_of!(super::sys::TcpInfo, delivery_rate), 160);
			// The app-limited flag shares byte 7 with `tcpi_fastopen_client_fail`.
			assert_eq!(std::mem::offset_of!(super::sys::TcpInfo, app_limited), 7);
			#[cfg(target_endian = "little")]
			assert_eq!(super::sys::APP_LIMITED_MASK, 0x01);
			#[cfg(target_endian = "big")]
			assert_eq!(super::sys::APP_LIMITED_MASK, 0x80);
		}

		/// Offsets from macOS `netinet/tcp.h`.
		#[cfg(target_os = "macos")]
		#[test]
		fn macos_tcp_connection_info_layout() {
			assert_eq!(std::mem::offset_of!(super::sys::TcpInfo, srtt), 44);
		}
	}
}

#[cfg(test)]
mod rate_filter_tests {
	use super::*;
	use kio::clock::Instant;

	fn at(start: Instant, ms: u64) -> Instant {
		start.saturating_add(Duration::from_millis(ms))
	}

	#[test]
	fn app_limited_samples_do_not_lower_the_estimate() {
		let t = kio::clock::now();
		let mut f = RateFilter::default();
		assert_eq!(f.update(t, 80_000_000, true), Some(80_000_000));
		// A live stream mostly trickles: these samples measure the sender, not the path.
		assert_eq!(f.update(at(t, 100), 1_000_000, true), Some(80_000_000));
		// Even after the window, an app-limited flow keeps the last estimate.
		assert_eq!(f.update(at(t, 20_000), 1_000_000, true), Some(80_000_000));
	}

	#[test]
	fn app_limited_samples_can_raise_the_estimate() {
		let t = kio::clock::now();
		let mut f = RateFilter::default();
		assert_eq!(f.update(t, 1_000_000, true), Some(1_000_000));
		assert_eq!(f.update(at(t, 100), 50_000_000, true), Some(50_000_000));
	}

	#[test]
	fn measured_samples_lower_it_after_the_window() {
		let t = kio::clock::now();
		let mut f = RateFilter::default();
		assert_eq!(f.update(t, 80_000_000, false), Some(80_000_000));
		// A real, lower measurement waits for the older maximum to expire.
		assert_eq!(f.update(at(t, 1_000), 5_000_000, false), Some(80_000_000));
		assert_eq!(f.update(at(t, 10_500), 5_000_000, false), Some(5_000_000));
	}

	#[test]
	fn app_limited_samples_do_not_expire_the_maximum() {
		let t = kio::clock::now();
		let mut f = RateFilter::default();
		f.update(t, 80_000_000, false);
		f.update(at(t, 1_000), 5_000_000, false);
		// Both measurements have expired, but an app-limited sample cannot retire them.
		assert_eq!(f.update(at(t, 20_000), 1_000_000, true), Some(80_000_000));
		assert_eq!(f.update(at(t, 30_000), 1_000_000, true), Some(80_000_000));
		// The next real measurement does.
		assert_eq!(f.update(at(t, 31_000), 4_000_000, false), Some(4_000_000));
	}

	#[test]
	fn the_window_keeps_the_largest_recent_measurement() {
		let t = kio::clock::now();
		let mut f = RateFilter::default();
		f.update(t, 30_000_000, false);
		f.update(at(t, 4_000), 20_000_000, false);
		f.update(at(t, 8_000), 10_000_000, false);
		assert_eq!(f.update(at(t, 10_500), 9_000_000, false), Some(20_000_000));
		assert_eq!(f.update(at(t, 14_500), 9_000_000, false), Some(10_000_000));
		assert_eq!(f.update(at(t, 18_500), 9_000_000, false), Some(9_000_000));
	}

	#[test]
	fn zero_rate_keeps_the_estimate() {
		let t = kio::clock::now();
		let mut f = RateFilter::default();
		// The kernel reports zero until it has a sample.
		assert_eq!(f.update(t, 0, false), None);
		assert_eq!(f.update(at(t, 100), 80_000_000, false), Some(80_000_000));
		assert_eq!(f.update(at(t, 20_000), 0, false), Some(80_000_000));
	}
}

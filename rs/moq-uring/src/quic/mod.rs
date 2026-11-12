//! QUIC connections over the worker's UDP path, as a MoQ transport.
//!
//! [`client::connect`] and [`server::accept`] wrap a sans-IO QUIC stack
//! around a [`udp::Socket`](crate::udp::Socket): a spawned driver task
//! shuttles packets between the socket and the connection (GSO trains out,
//! GRO coalesces in), arms the worker's userspace timers from the
//! connection's timeout, and wakes stream waiters. The returned
//! [`web::Session`] implements [`moq_net::transport::poll`], so wrapping
//! [`web::Session::raw`] in [`crate::transport::Local`] lets
//! `moq_net::Client::connect` / `Server::accept` run real MoQ sessions on the
//! worker; everything is `Rc`-shared and `!Send` by design.
//!
//! An [`Endpoint`] serves many connections on one socket, demuxed by
//! connection id; [`client::connect`] and [`server::accept`] are its
//! single-connection shorthands. Native peers speak raw QUIC (the ALPN
//! carries the application protocol); browsers negotiate `h3` and get the
//! [`web`] layer's HTTP/3 CONNECT handshake on top of the same adapter, with
//! [`web::Session`] as the one transport type covering both.
//!
//! moq-quic is the sans-IO QUIC stack underneath the worker.
//!
//! Session waits take `&self` and a caller-owned `&kio::Waiter`; stream reads and writes
//! take `&mut self`. Dropping the waiter cancels its registrations. Async helpers own their
//! waiter, while `Future` and runtime I/O traits bridge their `Context` through `kio::Park`.

pub mod client;
pub mod endpoint;
pub mod handshake;
#[cfg(feature = "qlog")]
pub mod qlog;
pub mod server;
pub mod transport;
pub mod web;

mod driver;

pub use driver::{Connection, recv, send};
pub use endpoint::Endpoint;
/// The flow-control limits and bitrate caps a live connection can change; see
/// [`Connection::set_limits`].
pub use moq_quic::{Limits, LimitsError, Rate};
/// Stream identity, and the varint every QUIC code is.
pub use moq_quic::{VarInt, stream};

/// A built-in congestion control family, which converts into the factory
/// [`transport::Config::congestion`] holds.
///
/// CUBIC is used for loss-based control and BBRv3 for delay-based control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Congestion {
	/// Loss-based: grows until it drops packets, so the send rate sawtooths.
	Loss,
	/// Delay-based: tracks the measured delivery rate and RTT instead of
	/// waiting for loss, which keeps queues short and the send rate steady
	/// enough for an encoder to track.
	Delay,
	/// No congestion control and no pacing, for tests and controlled links.
	None,
}

impl From<Congestion> for std::sync::Arc<dyn moq_congestion::Factory + Send + Sync> {
	fn from(family: Congestion) -> Self {
		match family {
			Congestion::Loss => std::sync::Arc::new(moq_congestion::cubic::Config::default()),
			Congestion::Delay => std::sync::Arc::new(moq_congestion::bbr3::Config::default()),
			Congestion::None => std::sync::Arc::new(moq_congestion::Unlimited),
		}
	}
}

/// Why a connection could not be set up or has ended.
///
/// A [`web::Session`] reports it as [`web::Error::Connection`], decoding the
/// peer's close code into the session's own.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The application (ours or the peer's) closed the connection.
	#[error("application closed: code={code} reason={reason:?}")]
	App {
		/// The application close code (a MoQ session code here).
		code: u64,
		/// The UTF-8 lossy close reason.
		reason: String,
	},
	/// QUIC closed the connection with a transport-level code.
	#[error("transport closed: code={code} reason={reason:?}")]
	Transport {
		/// The QUIC transport error code.
		code: u64,
		/// The UTF-8 lossy close reason.
		reason: String,
	},
	/// The connection idled out or the handshake never completed.
	#[error("connection timed out")]
	TimedOut,
	/// The peer reset the stream with this code.
	#[error("stream reset: {0}")]
	Reset(u64),
	/// The peer told us to stop sending with this code.
	#[error("stream stopped: {0}")]
	Stop(u64),
	/// The connection window was stuck and this stream was discarded to free
	/// it. See [`recv::Stream::poll_lease`]. The peer hears of it once the
	/// stream is stopped, so call `stop` with your own code: dropping the
	/// stream sends code 0.
	#[error("stream evicted")]
	Evicted,
	/// A lease wanted more than the connection receive window can hold.
	#[error("want exceeds the receive window")]
	TooLarge,
	/// This CPU lacks a feature that rind, MoQ's cryptography, requires.
	#[error(transparent)]
	UnsupportedCpu(#[from] moq_crypto::cpu::UnsupportedCpu),
	/// The socket died underneath the connection.
	#[error("socket error: {0}")]
	Io(String),
	/// The QUIC stack refused an operation. The backend's own message, since
	/// the two describe the same failures differently.
	#[error("quic error: {0}")]
	Quic(String),
	/// Accepting needs the server configuration the endpoint was built
	/// without.
	#[error("endpoint has no server configuration")]
	NotServer,
	/// qlog traces were asked for and cannot be captured: the directory is
	/// missing or unwritable, or the writer thread could not be started.
	#[error("qlog error: {0}")]
	Qlog(String),
	/// This endpoint closed the connection locally.
	#[error("locally closed")]
	Closed,
	/// The transport settings are ones the QUIC stack refuses.
	#[error("invalid quic config: {0}")]
	Config(#[from] moq_quic::ConfigError),
}

impl moq_net::transport::Error for Error {
	fn close_kind(&self) -> moq_net::transport::CloseKind {
		match self {
			Self::TimedOut => moq_net::transport::CloseKind::IdleTimeout,
			Self::Closed => moq_net::transport::CloseKind::Closed,
			Self::Evicted => moq_net::transport::CloseKind::Evicted,
			_ => moq_net::transport::CloseKind::Transport,
		}
	}
	fn session_error(&self) -> Option<(u32, String)> {
		match self {
			Self::App { code, reason } => u32::try_from(*code).ok().map(|code| (code, reason.clone())),
			_ => None,
		}
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			Self::Reset(code) | Self::Stop(code) => u32::try_from(*code).ok(),
			_ => None,
		}
	}
}

#[cfg(test)]
mod outcome_tests {
	use super::*;
	use moq_net::transport::{CloseKind, Error as _};
	#[test]
	fn transport_kinds_and_out_of_registry_codes_stay_typed() {
		assert_eq!(Error::TimedOut.close_kind(), CloseKind::IdleTimeout);
		assert_eq!(Error::Closed.close_kind(), CloseKind::Closed);
		assert_eq!(web::recv::Error::Evicted.close_kind(), CloseKind::Evicted);
		assert_eq!(
			web::send::Error::Session(web::Error::Connection(Error::TimedOut)).close_kind(),
			CloseKind::IdleTimeout
		);
		let outside = Error::App {
			code: u64::MAX,
			reason: "unknown registry".into(),
		};
		assert_eq!(outside.session_error(), None);
		assert_eq!(Error::Reset(u64::MAX).stream_error(), None);
	}
}

//! The per-connection transport knobs, the same for either role.

use super::Congestion;
#[cfg(feature = "qlog")]
use super::qlog;
use crate::quic::Limits;

/// The per-connection transport knobs, the same for either role.
///
/// Separate from the role configs so a caller that already has these
/// settings (a relay applying its `--quic-*` section, say) sets them once and
/// hands the same value to a dial and a listener.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Close the connection after this long without activity. Must fit a QUIC
	/// varint in milliseconds.
	pub idle_timeout: std::time::Duration,
	/// Flow-control windows and stream counts. Each `None` takes its default:
	///
	/// - 1024 streams of each kind a peer may have open at once. MoQ opens a
	///   stream per group, so busy endpoints want this high.
	/// - A 128 MiB connection window, the unread data and incomplete messages
	///   one peer may have in flight across every stream. A moq session needs
	///   it to fit the largest message it assembles,
	///   [`MIN_RECEIVE_WINDOW`](moq_net::transport::MIN_RECEIVE_WINDOW), or a
	///   partial one can stall the connection.
	/// - A 4 MiB stream window. Stream credit returns as bytes are read,
	///   whether or not their message is complete, so this only paces one
	///   stream.
	/// - 16 MiB of unacknowledged outgoing data, whatever the peer allows.
	pub limits: Limits,
	/// Which congestion controller to run.
	pub congestion: Congestion,
	/// How often to send an ack-eliciting packet on an otherwise idle
	/// connection, `None` (the default) to derive it from the negotiated idle
	/// timeout, or zero to send none and let the idle timeout decide. See
	/// [`moq_quic::transport::Config::keep_alive`].
	pub keep_alive: Option<std::time::Duration>,
	/// Probe for a path MTU above the 1200-byte floor every QUIC path carries,
	/// off by default. A probe the path drops costs only itself, and a path
	/// that later shrinks falls back to the floor.
	pub mtu_discovery: bool,
	/// Where to write qlog traces, or `None` (the default) to write none.
	///
	/// moq-quic takes one sink per transport config, so this writes one file
	/// per endpoint on the accept side and per dial on the connect side, each
	/// event tagged with the qlog `group_id` of the connection it belongs to.
	///
	/// Only compiled with the `qlog` feature, so a build without it cannot ask
	/// for traces the backends would not produce.
	#[cfg(feature = "qlog")]
	pub qlog: Option<qlog::Sink>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			idle_timeout: std::time::Duration::from_secs(10),
			limits: Limits::default(),
			congestion: Congestion::default(),
			keep_alive: None,
			mtu_discovery: false,
			#[cfg(feature = "qlog")]
			qlog: None,
		}
	}
}

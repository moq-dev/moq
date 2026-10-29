//! Session limits and QUIC limits selected after the relay authorizes a session.

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Session and transport limits grouped by the authorized connection's role.
#[derive(usage_rs::Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Session limits for ordinary clients.
	#[usage(flatten)]
	pub client: Client,
	/// Session and QUIC limits for authenticated cluster peers.
	#[usage(flatten)]
	pub peer: Peer,
	#[usage(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,
}

/// Session limits for ordinary client sessions.
#[derive(usage_rs::Args, Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Client {
	/// Maximum simultaneously announced broadcasts or namespaces.
	#[usage(
		long = "limits-client-announces",
		env = "MOQ_LIMITS_CLIENT_ANNOUNCES",
		default = "1000",
		setting = "limits.client.announces"
	)]
	pub announces: usize,
}

impl Default for Client {
	fn default() -> Self {
		Self { announces: 1000 }
	}
}

impl Client {
	pub(crate) fn resolve(&self) -> moq_net::session::Limits {
		resolve(self.announces)
	}
}

/// Session limits and QUIC limits for authenticated cluster peer sessions.
#[derive(usage_rs::Args, Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Peer {
	/// Maximum simultaneously announced broadcasts or namespaces.
	#[usage(
		long = "limits-peer-announces",
		env = "MOQ_LIMITS_PEER_ANNOUNCES",
		default = "100000",
		setting = "limits.peer.announces"
	)]
	pub announces: usize,
	/// QUIC limits a peer session widens to (`[limits.peer.quic]`).
	#[usage(flatten)]
	pub quic: Quic,
}

impl Default for Peer {
	fn default() -> Self {
		Self {
			announces: 100000,
			quic: Quic::default(),
		}
	}
}

impl Peer {
	pub(crate) fn resolve(&self) -> moq_net::session::Limits {
		resolve(self.announces)
	}
}

/// QUIC limits for peer sessions and the links the relay dials, keyed like `[quic]`.
#[derive(usage_rs::Args, Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Quic {
	/// Maximum concurrent QUIC streams the peer may open, per direction.
	#[usage(
		long = "limits-peer-quic-max-streams",
		env = "MOQ_LIMITS_PEER_QUIC_MAX_STREAMS",
		default = "100000",
		setting = "limits.peer.quic.max_streams"
	)]
	pub max_streams: std::num::NonZeroU64,
	/// Connection-wide QUIC receive window, in bytes.
	#[usage(
		long = "limits-peer-quic-receive-window",
		env = "MOQ_LIMITS_PEER_QUIC_RECEIVE_WINDOW",
		default = "671088640",
		setting = "limits.peer.quic.receive_window"
	)]
	pub receive_window: std::num::NonZeroU64,
	/// Cap on unacknowledged outgoing QUIC data, in bytes.
	#[usage(
		long = "limits-peer-quic-send-window",
		env = "MOQ_LIMITS_PEER_QUIC_SEND_WINDOW",
		default = "671088640",
		setting = "limits.peer.quic.send_window"
	)]
	pub send_window: std::num::NonZeroU64,
}

impl Default for Quic {
	fn default() -> Self {
		// Well above the `[quic]` defaults, which are sized for one viewer rather
		// than a link carrying a whole cluster's traffic.
		let window = std::num::NonZeroU64::new(640 << 20).expect("non-zero");
		Self {
			max_streams: std::num::NonZeroU64::new(10 * moq_tokio::quic::DEFAULT_MAX_STREAMS).expect("non-zero"),
			receive_window: window,
			send_window: window,
		}
	}
}

impl Quic {
	/// Refuses limits QUIC cannot advertise, as `[quic]` does, so the accept and
	/// dial paths never disagree about them.
	pub(crate) fn validate(&self) -> anyhow::Result<()> {
		self.limits().validate().context("invalid [limits.peer.quic]")?;
		// Incomplete messages hold connection credit until they are complete.
		let min = moq_net::transport::MIN_RECEIVE_WINDOW;
		anyhow::ensure!(
			self.receive_window.get() >= min,
			"invalid [limits.peer.quic]: receive_window must be at least {min} bytes to fit the largest message"
		);
		Ok(())
	}

	/// Apply these limits to the `[quic]` section a peer link is dialed with.
	pub(crate) fn dial(&self, quic: &mut moq_tokio::quic::Config) {
		quic.max_streams = Some(self.max_streams.get());
		quic.receive_window = Some(self.receive_window.get());
		quic.send_window = Some(self.send_window.get());
	}

	/// The limits an accepted peer connection widens to, as either QUIC runtime takes them.
	pub(crate) fn limits(&self) -> moq_quic::Limits {
		let mut limits = moq_quic::Limits::default();
		limits.max_streams = Some(self.max_streams);
		limits.receive_window = Some(self.receive_window);
		limits.send_window = Some(self.send_window);
		limits
	}
}

fn resolve(announces: usize) -> moq_net::session::Limits {
	let mut limits = moq_net::session::Limits::default();
	limits.announces = announces;
	limits
}

#[derive(usage_rs::Args, Clone, Debug, Default)]
pub(crate) struct Legacy {
	#[usage(long = "session-announces", env = "MOQ_SESSION_ANNOUNCES", hide = true)]
	session_announces: Option<u64>,
	#[usage(long = "peer-announces", env = "MOQ_PEER_ANNOUNCES", hide = true)]
	peer_announces: Option<u64>,
	#[usage(long = "peer-quic-max-streams", env = "MOQ_PEER_QUIC_MAX_STREAMS", hide = true)]
	peer_quic_max_streams: Option<u64>,
	#[usage(long = "peer-quic-receive-window", env = "MOQ_PEER_QUIC_RECEIVE_WINDOW", hide = true)]
	peer_quic_receive_window: Option<u64>,
	#[usage(long = "peer-quic-send-window", env = "MOQ_PEER_QUIC_SEND_WINDOW", hide = true)]
	peer_quic_send_window: Option<u64>,
	#[usage(long = "limits-client-requests", env = "MOQ_LIMITS_CLIENT_REQUESTS", hide = true)]
	client_requests: Option<u64>,
	#[usage(long = "limits-peer-requests", env = "MOQ_LIMITS_PEER_REQUESTS", hide = true)]
	peer_requests: Option<u64>,
}

impl Legacy {
	pub(crate) fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.session_announces.is_some() {
			found.flag(
				"--session-announces",
				Some("MOQ_SESSION_ANNOUNCES"),
				"--limits-client-announces",
			);
		}
		if self.peer_announces.is_some() {
			found.flag(
				"--peer-announces",
				Some("MOQ_PEER_ANNOUNCES"),
				"--limits-peer-announces",
			);
		}
		if self.peer_quic_max_streams.is_some() {
			found.flag(
				"--peer-quic-max-streams",
				Some("MOQ_PEER_QUIC_MAX_STREAMS"),
				"--limits-peer-quic-max-streams",
			);
		}
		if self.peer_quic_receive_window.is_some() {
			found.flag(
				"--peer-quic-receive-window",
				Some("MOQ_PEER_QUIC_RECEIVE_WINDOW"),
				"--limits-peer-quic-receive-window",
			);
		}
		if self.peer_quic_send_window.is_some() {
			found.flag(
				"--peer-quic-send-window",
				Some("MOQ_PEER_QUIC_SEND_WINDOW"),
				"--limits-peer-quic-send-window",
			);
		}
		const REQUESTS: &str = "stream flow control bounds requests";
		if self.client_requests.is_some() {
			found.removed("--limits-client-requests", Some("MOQ_LIMITS_CLIENT_REQUESTS"), REQUESTS);
		}
		if self.peer_requests.is_some() {
			found.removed("--limits-peer-requests", Some("MOQ_LIMITS_PEER_REQUESTS"), REQUESTS);
		}
		found
	}
}

use serde::{Deserialize, Serialize};
use serde_with::{DurationSecondsWithFrac, TimestampSeconds, serde_as};
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use crate::lease::Reason;

/// Everything a relay knows about a session, sent to the auth server on every event.
///
/// Nothing is parsed on the relay's behalf: the server keys policy on the raw
/// [`path`](Self::path) and [`query`](Self::query), so no query parameter is special
/// and a credential can be whatever the server understands. The same shape carries
/// every [`Event`]; an `end` adds what the session did.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct Request {
	/// Random 128-bit hex, unique per session; the key every event for it shares.
	pub id: String,

	/// Which lifecycle event this is, with what an `end` carries.
	#[serde(flatten)]
	pub event: Event,

	/// The operator's name for the relay asking.
	pub node: String,

	/// How the session reached the relay.
	pub transport: Transport,

	/// The peer's socket address, absent on a transport without one (a unix socket).
	pub remote: Option<SocketAddr>,

	/// The relay's socket address the session arrived on, absent likewise.
	pub local: Option<SocketAddr>,

	/// The SNI the client presented, when the transport carried TLS.
	pub server_name: Option<String>,

	/// The negotiated application protocol, including the moq version.
	pub alpn: Option<String>,

	/// The path exactly as dialed.
	pub path: String,

	/// The raw query string, without the leading `?`.
	pub query: Option<String>,

	/// The direction the client declared at SETUP; absent means both.
	pub role: Option<Role>,

	/// The verified client certificate, when one was presented.
	pub tls: Option<Peer>,
}

impl Request {
	/// A `connect` for a fresh session, minting a random 128-bit hex id.
	///
	/// Set the remaining fields on the returned value; the struct is
	/// `#[non_exhaustive]`, so this stays the way to build one as fields are added.
	pub fn new(node: impl Into<String>, transport: Transport, path: impl Into<String>) -> Self {
		let mut bytes = [0u8; 16];
		aws_lc_rs::rand::fill(&mut bytes).expect("failed to generate a session id");
		let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
		Self {
			id,
			event: Event::Connect,
			node: node.into(),
			transport,
			remote: None,
			local: None,
			server_name: None,
			alpn: None,
			path: path.into(),
			query: None,
			role: None,
			tls: None,
		}
	}
}

/// The lifecycle moment a [`Request`] reports.
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
	/// A session was accepted and asks to be admitted.
	Connect,
	/// The grant asked to be re-checked on its cadence.
	Revalidate,
	/// The session closed.
	End {
		/// Why it closed.
		reason: Reason,
		/// How long it was admitted.
		#[serde_as(as = "DurationSecondsWithFrac<f64>")]
		duration: Duration,
		/// What it moved.
		bytes: Bytes,
	},
}

/// How a session reached the relay. The names match `moq_tokio::server::Transport`,
/// plus `http` for the relay's one-shot HTTP routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
	/// QUIC, either directly or through WebTransport over HTTP/3.
	Quic,
	/// An Iroh QUIC connection.
	Iroh,
	/// A WebSocket connection using qmux framing.
	WebSocket,
	/// A plaintext TCP connection using qmux framing.
	Tcp,
	/// A Unix domain socket using qmux framing.
	Unix,
	/// A one-shot HTTP request on the relay's web listener (`/fetch`, `/announced`),
	/// admitted and ended within the request.
	Http,
}

impl Transport {
	/// The stable lowercase name, the same one the wire carries.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Quic => "quic",
			Self::Iroh => "iroh",
			Self::WebSocket => "websocket",
			Self::Tcp => "tcp",
			Self::Unix => "unix",
			Self::Http => "http",
		}
	}
}

impl std::fmt::Display for Transport {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

/// The single direction a client declared at SETUP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
	/// The client will publish; the relay consumes.
	Publisher,
	/// The client will subscribe; the relay publishes.
	Subscriber,
}

/// The verified client certificate a session presented, as facts for the server to
/// decide on. Presenting one admits nothing by itself.
#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Peer {
	/// The first SAN DNS name, else the CN, else the fingerprint, so it is never empty.
	///
	/// Those three sources fold into one string a server cannot tell apart; match on
	/// [`fingerprint`](Self::fingerprint) when identity must be exact.
	pub name: String,
	/// SHA-256 of the leaf certificate, hex.
	pub fingerprint: String,
	/// The certificate's notAfter.
	#[serde_as(as = "Option<TimestampSeconds<i64>>")]
	pub expires: Option<SystemTime>,
	/// The issuer's distinguished name.
	pub issuer: String,
}

/// Byte totals for a session, both directions from the relay's point of view.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bytes {
	/// Bytes the relay sent to the peer.
	pub sent: u64,
	/// Bytes the relay received from the peer.
	pub received: u64,
}

#[cfg(test)]
mod tests {
	use super::*;

	fn request() -> Request {
		let mut request = Request::new("relay-1", Transport::Quic, "/demo/room");
		request.id = "00ff".into();
		request.remote = Some("203.0.113.9:4433".parse().unwrap());
		request.local = Some("[::1]:443".parse().unwrap());
		request.server_name = Some("relay.example".into());
		request.alpn = Some("moq-lite-05".into());
		request.query = Some("jwt=abc".into());
		request.role = Some(Role::Publisher);
		request.tls = Some(Peer {
			name: "edge0".into(),
			fingerprint: "ab".repeat(32),
			expires: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
			issuer: "CN=cluster".into(),
		});
		request
	}

	#[test]
	fn connect_round_trips_flat() {
		let request = request();
		let json = serde_json::to_value(&request).unwrap();
		assert_eq!(json["event"], "connect");
		assert_eq!(json["transport"], "quic");
		assert_eq!(json["role"], "publisher");
		assert_eq!(json["remote"], "203.0.113.9:4433");
		assert_eq!(json["tls"]["expires"], 4_102_444_800_i64);
		assert!(json.get("reason").is_none());
		assert_eq!(serde_json::from_value::<Request>(json).unwrap(), request);
	}

	#[test]
	fn end_carries_its_facts_beside_the_rest() {
		let mut request = request();
		request.event = Event::End {
			reason: Reason::Session("disconnected".into()),
			duration: Duration::from_millis(1500),
			bytes: Bytes { sent: 10, received: 20 },
		};
		let json = serde_json::to_value(&request).unwrap();
		assert_eq!(json["event"], "end");
		assert_eq!(json["reason"], "disconnected");
		assert_eq!(json["duration"], 1.5);
		assert_eq!(json["bytes"]["sent"], 10);
		assert_eq!(serde_json::from_value::<Request>(json).unwrap(), request);
	}

	/// The exact bytes `js/auth/src/interop.test.ts` parses, so both languages read
	/// one wire shape.
	#[test]
	fn end_serializes_to_the_cross_language_vector() {
		let mut request = Request::new("relay-1", Transport::WebSocket, "/demo/room");
		request.id = "00ff".into();
		request.remote = Some("203.0.113.9:4433".parse().unwrap());
		request.query = Some("jwt=abc".into());
		request.event = Event::End {
			reason: Reason::Expired,
			duration: Duration::from_millis(1500),
			bytes: Bytes { sent: 10, received: 20 },
		};
		assert_eq!(
			serde_json::to_string(&request).unwrap(),
			r#"{"id":"00ff","event":"end","reason":"expired","duration":1.5,"bytes":{"sent":10,"received":20},"node":"relay-1","transport":"websocket","remote":"203.0.113.9:4433","path":"/demo/room","query":"jwt=abc"}"#
		);
	}

	#[test]
	fn a_unix_session_has_no_addresses() {
		let request = Request::new("relay-1", Transport::Unix, "");
		let json = serde_json::to_value(&request).unwrap();
		assert!(json.get("remote").is_none());
		assert_eq!(json["transport"], "unix");
		assert_eq!(serde_json::from_value::<Request>(json).unwrap(), request);
	}

	#[test]
	fn new_mints_a_128_bit_hex_id() {
		let a = Request::new("relay-1", Transport::Quic, "/");
		let b = Request::new("relay-1", Transport::Quic, "/");
		assert_eq!(a.id.len(), 32);
		assert!(a.id.chars().all(|c| c.is_ascii_hexdigit()), "{}", a.id);
		assert_ne!(a.id, b.id);
	}
}

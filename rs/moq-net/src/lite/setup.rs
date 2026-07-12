//! The lite-05 SETUP message: each endpoint advertises its capabilities once, as
//! the sole message on a unidirectional Setup Stream, then closes it.

use crate::coding::*;

use super::{Message, Parameters, Version};

/// Setup Parameter id for the Probe capability level.
const PARAM_PROBE: u64 = 0x1;
/// Setup Parameter id for the request Path (client-only, URI-less transports).
const PARAM_PATH: u64 = 0x2;
/// Setup Parameter id for the client's intended [`Role`] (client-only).
const PARAM_ROLE: u64 = 0x3;

/// The probe capability an endpoint advertises in SETUP.
///
/// Monotonic: a higher level implies every lower one. An unknown (future) value
/// decodes as the highest level we understand, so a peer that gains a new level is
/// treated as at least [`Increase`](Self::Increase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ProbeLevel {
	/// No probing. Equivalent to omitting the parameter.
	#[default]
	None,
	/// The publisher can measure and periodically report its estimated bitrate.
	Report,
	/// The publisher can additionally pad the connection (or send redundant data).
	Increase,
}

impl ProbeLevel {
	/// Map the wire value to a level, saturating unknown values to [`Increase`](Self::Increase).
	fn from_code(code: u64) -> Self {
		match code {
			0 => Self::None,
			1 => Self::Report,
			_ => Self::Increase,
		}
	}

	/// The wire value for this level.
	fn to_code(self) -> u64 {
		match self {
			Self::None => 0,
			Self::Report => 1,
			Self::Increase => 2,
		}
	}
}

/// The direction a client intends to use the session for.
///
/// A client advertises this in its SETUP so the server can reject a token that lacks
/// the matching scope during the handshake, instead of accepting a connection that
/// then silently carries no media (a subscribe-only token used to publish, or vice
/// versa). It only ever narrows what the server grants, so it is not a security
/// boundary: the server still enforces the token's scope regardless.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
	/// The client will publish tracks (ingest); the server must consume.
	Publisher,
	/// The client will subscribe to tracks (egress); the server must publish.
	Subscriber,
	/// The client may do either, or declined to say. The default, and the behavior of
	/// clients that predate this parameter (they omit it, and it decodes back to this).
	#[default]
	Both,
}

impl Role {
	/// Map the wire value to a role. `0` and any unrecognized future value fall back to
	/// [`Both`](Role::Both): the draft requires a receiver that does not recognize the
	/// value to treat it as `Both`, so a newer client can't break an older server (it
	/// just loses the early reject and defers fully to the token's scope).
	fn from_code(code: u64) -> Self {
		match code {
			1 => Role::Publisher,
			2 => Role::Subscriber,
			_ => Role::Both,
		}
	}

	/// The wire value for a directional role, or `None` for [`Role::Both`], which is the
	/// absence of the parameter and so is never encoded.
	fn to_code(self) -> Option<u64> {
		match self {
			Role::Publisher => Some(1),
			Role::Subscriber => Some(2),
			Role::Both => None,
		}
	}

	/// Derive the advertised role from which origins a client wired up: publish-only is
	/// a [`Publisher`](Role::Publisher), consume-only a [`Subscriber`](Role::Subscriber),
	/// and both (or neither) stays [`Both`](Role::Both). This keeps the advertised role
	/// from drifting away from what the session actually does.
	pub(crate) fn from_origins(publishes: bool, consumes: bool) -> Self {
		match (publishes, consumes) {
			(true, false) => Role::Publisher,
			(false, true) => Role::Subscriber,
			_ => Role::Both,
		}
	}
}

/// The SETUP message, sent once per endpoint on the unidirectional Setup Stream.
///
/// lite-05+ only. The two endpoints' SETUP messages are independent: neither side
/// blocks on the peer's before opening other streams, but a stream whose encoding
/// depends on a negotiated capability (e.g. PROBE) must wait for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Setup {
	/// The probe capability this endpoint supports. [`ProbeLevel::None`] when absent.
	pub probe: ProbeLevel,
	/// The request path, for transports that carry no request URI (native QUIC,
	/// qmux over TCP/TLS). Sent only by the client; a server never sends one and a
	/// relay never forwards it. `None` on URI-carrying bindings.
	pub path: Option<String>,
	/// The client's intended [`Role`]. `Both` (the default) is sent as the absence of
	/// the parameter, so an old client that never sets it decodes back to `Both`.
	pub role: Role,
}

impl Message for Setup {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_setup_stream() {
			return Err(DecodeError::Version);
		}

		let params = Parameters::decode(r, version)?;
		let probe = params
			.get_varint(PARAM_PROBE)?
			.map(ProbeLevel::from_code)
			.unwrap_or_default();
		let path = match params.get_bytes(PARAM_PATH) {
			Some(bytes) => {
				let s = std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidValue)?;
				if s.is_empty() {
					return Err(DecodeError::InvalidValue);
				}
				Some(s.to_string())
			}
			None => None,
		};
		let role = params.get_varint(PARAM_ROLE)?.map(Role::from_code).unwrap_or_default();

		Ok(Self { probe, path, role })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_setup_stream() {
			return Err(EncodeError::Version);
		}

		let mut params = Parameters::default();
		// None is the wire default, so omit it to keep the message empty when nothing is set.
		if self.probe != ProbeLevel::None {
			params.set_varint(PARAM_PROBE, self.probe.to_code());
		}
		if let Some(path) = &self.path {
			params.set_bytes(PARAM_PATH, path.as_bytes().to_vec());
		}
		// Both is the wire default (absence of the parameter), so only a directional role is encoded.
		if let Some(code) = self.role.to_code() {
			params.set_varint(PARAM_ROLE, code);
		}

		params.encode(w, version)
	}
}

/// Shared slot for the peer's SETUP, written once when its Setup stream is read.
///
/// Streams whose encoding depends on a negotiated capability (e.g. the PROBE
/// stream) wait on this before deciding what to do. Cheap to clone: every handle
/// shares the same watch channel.
#[derive(Clone)]
pub(crate) struct PeerSetup(tokio::sync::watch::Sender<Option<Setup>>);

impl Default for PeerSetup {
	fn default() -> Self {
		Self(tokio::sync::watch::channel(None).0)
	}
}

impl PeerSetup {
	/// Record the peer's SETUP.
	pub fn set(&self, setup: Setup) {
		// Ignored if every receiver has dropped; nothing is waiting on it then.
		let _ = self.0.send(Some(setup));
	}

	/// Await the peer's advertised probe level, blocking until its SETUP arrives.
	///
	/// The peer MUST send exactly one SETUP, so this resolves once that stream is read.
	pub async fn probe_level(&self) -> ProbeLevel {
		let mut rx = self.0.subscribe();
		loop {
			// Clone out of the borrow before awaiting so no guard crosses the await point.
			if let Some(setup) = rx.borrow_and_update().clone() {
				return setup.probe;
			}
			if rx.changed().await.is_err() {
				// Sender dropped before sending: treat as no probe support.
				return ProbeLevel::default();
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn round_trip(msg: &Setup) -> Setup {
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, Version::Lite05).unwrap();
		let mut slice = &buf[..];
		let got = Setup::decode(&mut slice, Version::Lite05).unwrap();
		assert!(bytes::Buf::remaining(&slice) == 0, "trailing bytes after decode");
		got
	}

	#[test]
	fn empty_round_trip() {
		let msg = Setup::default();
		assert_eq!(round_trip(&msg), msg);
	}

	#[test]
	fn probe_levels_round_trip() {
		for probe in [ProbeLevel::None, ProbeLevel::Report, ProbeLevel::Increase] {
			let msg = Setup {
				probe,
				..Default::default()
			};
			assert_eq!(round_trip(&msg), msg);
		}
	}

	#[test]
	fn path_round_trip() {
		let msg = Setup {
			probe: ProbeLevel::Report,
			path: Some("/room/123".to_string()),
			role: Role::Both,
		};
		assert_eq!(round_trip(&msg), msg);
	}

	#[test]
	fn roles_round_trip() {
		for role in [Role::Publisher, Role::Subscriber, Role::Both] {
			let msg = Setup {
				path: Some("/room/123".to_string()),
				role,
				..Default::default()
			};
			assert_eq!(round_trip(&msg), msg);
		}
	}

	#[test]
	fn role_both_omits_parameter() {
		// Both is the absence of the parameter, so a SETUP is byte-identical whether the
		// role is set to Both or left at its default. This is what lets a client that
		// predates the parameter decode back to Both.
		let mut with = bytes::BytesMut::new();
		Setup {
			role: Role::Both,
			..Default::default()
		}
		.encode(&mut with, Version::Lite05)
		.unwrap();

		let mut without = bytes::BytesMut::new();
		Setup::default().encode(&mut without, Version::Lite05).unwrap();

		assert_eq!(with, without);
	}

	#[test]
	fn unknown_probe_level_saturates_to_increase() {
		// Frame a SETUP message carrying an unknown probe level (99) by hand: the
		// parameters body, prefixed with its length (the lite Message size prefix).
		let mut params = Parameters::default();
		params.set_varint(PARAM_PROBE, 99);
		let mut body = Vec::new();
		params.encode(&mut body, Version::Lite05).unwrap();

		let mut buf = bytes::BytesMut::new();
		body.len().encode(&mut buf, Version::Lite05).unwrap();
		buf.extend_from_slice(&body);

		let mut slice = &buf[..];
		let got = Setup::decode(&mut slice, Version::Lite05).unwrap();
		assert_eq!(got.probe, ProbeLevel::Increase);
	}

	#[test]
	fn unknown_role_decodes_as_both() {
		// A role value the receiver doesn't recognize (a future extension, or an explicit
		// 0) decodes to Both rather than failing, so a newer client can't break an older
		// server. The draft mandates this fallback.
		for code in [0u64, 9, 250] {
			let mut params = Parameters::default();
			params.set_varint(PARAM_ROLE, code);
			let mut body = Vec::new();
			params.encode(&mut body, Version::Lite05).unwrap();

			let mut buf = bytes::BytesMut::new();
			body.len().encode(&mut buf, Version::Lite05).unwrap();
			buf.extend_from_slice(&body);

			let mut slice = &buf[..];
			let got = Setup::decode(&mut slice, Version::Lite05).unwrap();
			assert_eq!(got.role, Role::Both, "role code {code} should decode as Both");
		}
	}

	#[test]
	fn rejects_before_lite05() {
		let msg = Setup::default();
		let mut buf = bytes::BytesMut::new();
		assert!(matches!(
			msg.encode(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));
	}

	#[test]
	fn ignores_unknown_parameters() {
		// Frame a SETUP carrying an unknown parameter ID alongside the path.
		let mut params = Parameters::default();
		params.set_bytes(PARAM_PATH, b"/foo".to_vec());
		params.set_bytes(0xbeef, b"whatever".to_vec());

		let mut body = Vec::new();
		params.encode(&mut body, Version::Lite05).unwrap();

		// Wrap with the message size prefix the Message impl expects.
		let mut buf = bytes::BytesMut::new();
		body.len().encode(&mut buf, Version::Lite05).unwrap();
		buf.extend_from_slice(&body);

		let mut slice = &buf[..];
		let got = Setup::decode(&mut slice, Version::Lite05).unwrap();
		assert_eq!(got.path.as_deref(), Some("/foo"));
	}
}

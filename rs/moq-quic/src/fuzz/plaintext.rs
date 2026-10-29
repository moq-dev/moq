//! Crypto with no TLS and no packet protection, for fuzzing and benchmarks.
//!
//! The handshake keeps TLS 1.3's shape so every packet space carries CRYPTO data: each
//! side's hello holds its transport parameters and goes out in Initial, then the server's
//! finished and the client's go out in Handshake. A message is a varint length and its
//! body. Packets leave in the clear with a zeroed tag the size of AES-GCM's, so they are
//! as large as real ones, and any packet with room for a tag decrypts.

use std::{any::Any, sync::Arc};

use bytes::BytesMut;

use crate::{
	ConnectionId, Side, TransportError, Version,
	coding::{BufExt, BufMutExt},
	crypto::{self, CryptoError, ExportKeyingMaterialError, HeaderKey, KeyPair, Keys, PacketKey},
	endpoint::ConnectError,
	transport_parameters::TransportParameters,
};

/// The bytes a packet reserves for its tag, and the header protection sample size.
pub(super) const TAG_LEN: usize = 16;

/// The two messages each side sends: its hello, then its finished.
const MESSAGES: usize = 2;

/// Client and server crypto that skips TLS and leaves every packet in the clear.
///
/// Pass it to `client::Config::new` and `server::Config::new`.
pub struct Plaintext;

impl crypto::ClientConfig for Plaintext {
	fn start_session(
		self: Arc<Self>,
		_version: Version,
		_server_name: &str,
		params: &TransportParameters,
	) -> Result<Box<dyn crypto::Session>, ConnectError> {
		Ok(Box::new(Session::new(Side::Client, params)))
	}
}

impl crypto::ServerConfig for Plaintext {
	fn initial_keys(&self, _version: Version, _dst_cid: ConnectionId) -> Keys {
		keys()
	}

	fn retry_tag(&self, _version: Version, _orig_dst_cid: ConnectionId, _packet: &[u8]) -> [u8; 16] {
		[0; 16]
	}

	fn start_session(self: Arc<Self>, _version: Version, params: &TransportParameters) -> Box<dyn crypto::Session> {
		Box::new(Session::new(Side::Server, params))
	}
}

struct Session {
	side: Side,
	/// This side's encoded transport parameters, the body of its hello.
	hello: Vec<u8>,
	/// The peer's transport parameters, once its hello arrives.
	peer: Option<TransportParameters>,
	/// Handshake bytes that do not yet make a whole message.
	inbox: Vec<u8>,
	/// Handshake messages this side has written and the peer's it has read.
	sent: usize,
	received: usize,
	/// The packet spaces keys were handed out for past Initial.
	upgrades: usize,
}

impl Session {
	fn new(side: Side, params: &TransportParameters) -> Self {
		let mut hello = Vec::new();
		params.write(&mut hello);
		Self {
			side,
			hello,
			peer: None,
			inbox: Vec::new(),
			sent: 0,
			received: 0,
			upgrades: 0,
		}
	}

	fn receive(&mut self, body: &[u8]) -> Result<(), TransportError> {
		match self.received {
			0 => self.peer = Some(TransportParameters::read(self.side, &mut &body[..])?),
			// A finished answers this side's hello, and carries nothing.
			1 if self.sent > 0 && body.is_empty() => {}
			_ => {
				return Err(TransportError::PROTOCOL_VIOLATION(
					"unexpected plaintext handshake message",
				));
			}
		}
		self.received += 1;
		Ok(())
	}
}

impl crypto::Session for Session {
	fn initial_keys(&self, _dst_cid: ConnectionId, _side: Side) -> Keys {
		keys()
	}

	fn handshake_data(&self) -> Option<Box<dyn Any>> {
		None
	}

	fn peer_identity(&self) -> Option<Box<dyn Any>> {
		None
	}

	fn is_handshaking(&self) -> bool {
		self.received < MESSAGES
	}

	fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError> {
		if self.received == MESSAGES {
			return Err(TransportError::PROTOCOL_VIOLATION(
				"plaintext handshake data after the handshake",
			));
		}

		self.inbox.extend_from_slice(buf);
		loop {
			let mut rest = &self.inbox[..];
			let Ok(len) = rest.get_var() else {
				break;
			};
			let Some(body) = usize::try_from(len).ok().and_then(|len| rest.get(..len)) else {
				break;
			};
			let consumed = self.inbox.len() - rest.len() + body.len();
			let body = body.to_vec();
			self.inbox.drain(..consumed);
			self.receive(&body)?;
		}
		Ok(false)
	}

	fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError> {
		Ok(self.peer)
	}

	fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys> {
		// What is written goes out in the space this side was in before the call; returning
		// keys moves it to the next one.
		let (message, upgrade): (Option<&[u8]>, bool) = match (self.side, self.sent, self.received, self.upgrades) {
			(Side::Client, 0, _, _) => (Some(&self.hello), false),
			(Side::Client, 1, 1.., 0) => (None, true),
			(Side::Client, 1, 2, 1) => (Some(&[]), true),
			(Side::Server, 0, 1.., 0) => (Some(&self.hello), true),
			(Side::Server, 1, _, 1) => (Some(&[]), true),
			_ => (None, false),
		};
		if let Some(body) = message {
			buf.write_var(body.len() as u64);
			buf.extend_from_slice(body);
			self.sent += 1;
		}
		upgrade.then(|| {
			self.upgrades += 1;
			keys()
		})
	}

	fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn PacketKey>>> {
		Some(KeyPair {
			local: Box::new(Clear),
			remote: Box::new(Clear),
		})
	}

	fn is_valid_retry(&self, _orig_dst_cid: ConnectionId, _header: &[u8], payload: &[u8]) -> bool {
		payload
			.len()
			.checked_sub(16)
			.is_some_and(|tag| payload[tag..] == [0; 16])
	}

	fn export_keying_material(
		&self,
		_output: &mut [u8],
		_label: &[u8],
		_context: &[u8],
	) -> Result<(), ExportKeyingMaterialError> {
		Err(ExportKeyingMaterialError)
	}
}

fn keys() -> Keys {
	Keys {
		header: KeyPair {
			local: Box::new(Clear),
			remote: Box::new(Clear),
		},
		packet: KeyPair {
			local: Box::new(Clear),
			remote: Box::new(Clear),
		},
	}
}

/// A header and packet key that leaves the bytes alone.
pub(super) struct Clear;

impl HeaderKey for Clear {
	fn decrypt(&self, _pn_offset: usize, _packet: &mut [u8]) {}

	fn encrypt(&self, _pn_offset: usize, _packet: &mut [u8]) {}

	fn sample_size(&self) -> usize {
		TAG_LEN
	}
}

impl PacketKey for Clear {
	fn encrypt(&self, _packet: u64, buf: &mut [u8], _header_len: usize) {
		let tag = buf.len() - TAG_LEN;
		buf[tag..].fill(0);
	}

	/// Strips the tag without checking it, so a corrupted packet still reaches the frames.
	fn decrypt(&self, _packet: u64, _header: &[u8], payload: &mut BytesMut) -> Result<(), CryptoError> {
		let len = payload.len().checked_sub(TAG_LEN).ok_or(CryptoError)?;
		payload.truncate(len);
		Ok(())
	}

	fn tag_len(&self) -> usize {
		TAG_LEN
	}

	// Never forcing a key update, which a forged packet could not follow.
	fn confidentiality_limit(&self) -> u64 {
		u64::MAX
	}

	fn integrity_limit(&self) -> u64 {
		u64::MAX
	}
}

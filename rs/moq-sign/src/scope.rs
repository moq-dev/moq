//! The signed message: what a frame's signature binds besides its payload.

use std::sync::Arc;

use bytes::{BufMut, Bytes, BytesMut};

use crate::error::{Error, Result};
use crate::key::{PublicKey, SecretKey};

/// Length of the Ed25519 signature appended to every signed frame.
pub(crate) const SIGNATURE_LEN: usize = 64;

/// Domain separation for the profile, with no length prefix of its own.
const LABEL: &[u8] = b"moq-sign-00";

/// One track of one publisher instance: the identity every frame on it is signed under.
#[derive(Debug)]
pub(crate) struct Scope {
	// The canonical prefix: label, path, epoch, and track, each length-prefixed.
	prefix: Vec<u8>,
}

impl Scope {
	pub(crate) fn new(path: &moq_net::Path, epoch: &moq_net::Epoch, track: &str) -> Arc<Self> {
		let mut prefix = Vec::with_capacity(LABEL.len() + 24 + path.as_str().len() + 36 + track.len());
		prefix.extend_from_slice(LABEL);
		for field in [path.as_str(), epoch.as_str(), track] {
			prefix.extend_from_slice(&(field.len() as u64).to_be_bytes());
			prefix.extend_from_slice(field.as_bytes());
		}
		Arc::new(Self { prefix })
	}

	/// The message signed for `payload` at frame `frame` of group `group`, presented at `timestamp`.
	///
	/// The timestamp is its value at the track's timescale, so a frame keeps its signature only
	/// on a hop that carries both unchanged. An untimed frame binds zero for both, which no
	/// timescale can be.
	pub(crate) fn message(
		&self,
		group: u64,
		frame: u64,
		timestamp: Option<moq_net::Timestamp>,
		payload: &[u8],
	) -> Vec<u8> {
		let (timescale, value) = timestamp.map_or((0, 0), |at| (at.scale().as_u64(), at.value()));
		let mut message = Vec::with_capacity(self.prefix.len() + 32 + payload.len());
		message.extend_from_slice(&self.prefix);
		for field in [group, frame, timescale, value] {
			message.extend_from_slice(&field.to_be_bytes());
		}
		message.extend_from_slice(payload);
		message
	}

	/// The frame body: `payload` followed by its signature.
	pub(crate) fn sign(
		&self,
		key: &SecretKey,
		group: u64,
		frame: u64,
		timestamp: Option<moq_net::Timestamp>,
		payload: &[u8],
	) -> Bytes {
		let signature = key.sign(&self.message(group, frame, timestamp, payload));
		let mut body = BytesMut::with_capacity(payload.len() + SIGNATURE_LEN);
		body.put_slice(payload);
		body.put_slice(&signature);
		body.freeze()
	}

	/// The payload of a frame body whose signature verifies, without copying it.
	pub(crate) fn verify(
		&self,
		key: &PublicKey,
		group: u64,
		frame: u64,
		timestamp: Option<moq_net::Timestamp>,
		body: Bytes,
	) -> Result<Bytes> {
		let split = body.len().checked_sub(SIGNATURE_LEN).ok_or(Error::Signature)?;
		let signature: &[u8; SIGNATURE_LEN] = body[split..].try_into().expect("the signature's length");
		if !key.verify(&self.message(group, frame, timestamp, &body[..split]), signature) {
			return Err(Error::Signature);
		}
		Ok(body.slice(..split))
	}
}

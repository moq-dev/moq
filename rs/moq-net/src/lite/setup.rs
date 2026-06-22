//! The lite-05 SETUP message: each endpoint advertises its capabilities once, as
//! the sole message on a unidirectional Setup Stream, then closes it.

use crate::Compression;
use crate::coding::*;

use super::{Message, Parameters, Version};

/// Setup Parameter id for the Probe capability level.
const PARAM_PROBE: u64 = 0x1;
/// Setup Parameter id for the request Path (client-only, URI-less transports).
const PARAM_PATH: u64 = 0x2;
/// Setup Parameter id for the compression algorithms this endpoint can decompress.
const PARAM_COMPRESSION: u64 = 0x3;

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
	/// Compression algorithms this endpoint can *decompress*, in preference order
	/// (most-preferred first). Governs only what a peer may compress when sending
	/// *to* us; the sender names the algorithm actually used per frame. `none` (0)
	/// is never listed. Empty (the default) means "send me everything verbatim".
	pub compression: Vec<Compression>,
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

		// A back-to-back sequence of algorithm varints. Skip `none` (0) and any
		// identifier we don't understand: we can neither produce nor consume it.
		let mut compression = Vec::new();
		if let Some(bytes) = params.get_bytes(PARAM_COMPRESSION) {
			let mut slice = bytes;
			while !slice.is_empty() {
				let code = u64::decode(&mut slice, version)?;
				if let Ok(algo) = Compression::from_code(code)
					&& algo != Compression::None
					&& !compression.contains(&algo)
				{
					compression.push(algo);
				}
			}
		}

		Ok(Self {
			probe,
			path,
			compression,
		})
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
		// Pack the advertised algorithms back-to-back as varints, omitting `none`.
		let mut algos = Vec::new();
		for algo in &self.compression {
			if *algo != Compression::None {
				algo.to_code().encode(&mut algos, version)?;
			}
		}
		if !algos.is_empty() {
			params.set_bytes(PARAM_COMPRESSION, algos);
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

	/// Await the algorithms the peer can decompress, blocking until its SETUP arrives.
	///
	/// A publisher consults this before compressing: it MUST NOT use an algorithm the
	/// peer did not advertise. An empty list (no parameter, or the sender dropped
	/// without a SETUP) means everything must be sent verbatim.
	pub async fn compression(&self) -> Vec<Compression> {
		let mut rx = self.0.subscribe();
		loop {
			// Clone out of the borrow before awaiting so no guard crosses the await point.
			if let Some(setup) = rx.borrow_and_update().clone() {
				return setup.compression;
			}
			if rx.changed().await.is_err() {
				return Vec::new();
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn round_trip(msg: &Setup) -> Setup {
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = &buf[..];
		let got = Setup::decode(&mut slice, Version::Lite05Wip).unwrap();
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
				path: None,
				compression: Vec::new(),
			};
			assert_eq!(round_trip(&msg), msg);
		}
	}

	#[test]
	fn path_round_trip() {
		let msg = Setup {
			probe: ProbeLevel::Report,
			path: Some("/room/123".to_string()),
			compression: Vec::new(),
		};
		assert_eq!(round_trip(&msg), msg);
	}

	#[test]
	fn compression_round_trip() {
		let msg = Setup {
			probe: ProbeLevel::None,
			path: None,
			compression: vec![Compression::Deflate],
		};
		assert_eq!(round_trip(&msg), msg);
	}

	#[test]
	fn compression_decode_skips_none_and_unknown() {
		// Hand-frame a SETUP whose Compression parameter lists none (0), deflate (1),
		// and an unknown algorithm (99); only deflate survives the decode.
		let mut algos = Vec::new();
		for code in [0u64, 1, 99] {
			code.encode(&mut algos, Version::Lite05Wip).unwrap();
		}
		let mut params = Parameters::default();
		params.set_bytes(PARAM_COMPRESSION, algos);
		let mut body = Vec::new();
		params.encode(&mut body, Version::Lite05Wip).unwrap();

		let mut buf = bytes::BytesMut::new();
		body.len().encode(&mut buf, Version::Lite05Wip).unwrap();
		buf.extend_from_slice(&body);

		let mut slice = &buf[..];
		let got = Setup::decode(&mut slice, Version::Lite05Wip).unwrap();
		assert_eq!(got.compression, vec![Compression::Deflate]);
	}

	#[test]
	fn unknown_probe_level_saturates_to_increase() {
		// Frame a SETUP message carrying an unknown probe level (99) by hand: the
		// parameters body, prefixed with its length (the lite Message size prefix).
		let mut params = Parameters::default();
		params.set_varint(PARAM_PROBE, 99);
		let mut body = Vec::new();
		params.encode(&mut body, Version::Lite05Wip).unwrap();

		let mut buf = bytes::BytesMut::new();
		body.len().encode(&mut buf, Version::Lite05Wip).unwrap();
		buf.extend_from_slice(&body);

		let mut slice = &buf[..];
		let got = Setup::decode(&mut slice, Version::Lite05Wip).unwrap();
		assert_eq!(got.probe, ProbeLevel::Increase);
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
}

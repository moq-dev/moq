//! QMux transport parameters, carried in QX_TRANSPORT_PARAMETERS instead of a TLS extension
//!
//! The standard parameters are QUIC v1's and decode through [`TransportParameters`]. QMux adds
//! a pass over every parameter that rejects the QUIC v1 ones it prohibits and any duplicate,
//! and extracts its own `max_record_size` and `application_protocols`, which the shared codec
//! would skip as unknown.

use std::collections::HashSet;

use bytes::{Buf, BufMut, Bytes};

use super::Version;
use crate::{Side, TransportError, VarInt, coding::BufMutExt, transport_parameters::TransportParameters};

const MAX_IDLE_TIMEOUT: u64 = 0x01;
const INITIAL_MAX_DATA: u64 = 0x04;
const INITIAL_MAX_STREAM_DATA_BIDI_LOCAL: u64 = 0x05;
const INITIAL_MAX_STREAM_DATA_BIDI_REMOTE: u64 = 0x06;
const INITIAL_MAX_STREAM_DATA_UNI: u64 = 0x07;
const INITIAL_MAX_STREAMS_BIDI: u64 = 0x08;
const INITIAL_MAX_STREAMS_UNI: u64 = 0x09;
const RESET_STREAM_AT: u64 = 0x1d;
const MAX_DATAGRAM_FRAME_SIZE: u64 = 0x20;
const MAX_RECORD_SIZE: u64 = 0x0571c59429cd0845;
/// The non-TLS substitute for ALPN: each side lists its protocols and the server's preference
/// wins
const APPLICATION_PROTOCOLS: u64 = 0x3d4f9c2a8b1e6075;

/// The default and minimum `max_record_size`: 16 KiB less a 2-byte Size field
pub(super) const DEFAULT_MAX_RECORD_SIZE: u64 = 16382;

/// The QUIC v1 parameters QMux prohibits: original_destination_connection_id,
/// stateless_reset_token, max_udp_payload_size, ack_delay_exponent, max_ack_delay,
/// disable_active_migration, preferred_address, active_connection_id_limit,
/// initial_source_connection_id, and retry_source_connection_id
fn is_forbidden(id: u64) -> bool {
	matches!(id, 0x00 | 0x02 | 0x03 | 0x0a..=0x10)
}

/// The parameters one endpoint advertises
#[derive(Debug, Clone)]
pub(super) struct Params {
	/// The standard QUIC parameters: flow control, stream limits, idle timeout, datagrams, and
	/// RESET_STREAM_AT
	pub(super) tp: TransportParameters,
	/// The largest record Size field the sender accepts
	pub(super) max_record_size: VarInt,
	/// The application protocols offered for in-band negotiation, absent when the parameter is
	pub(super) protocols: Option<Vec<String>>,
}

fn err(reason: &'static str) -> TransportError {
	TransportError::TRANSPORT_PARAMETER_ERROR(reason)
}

impl Params {
	/// Encode in the order qmux 0.6 did, so both produce the same bytes
	pub(super) fn encode(&self, buf: &mut Vec<u8>) {
		fn varint(buf: &mut Vec<u8>, id: u64, value: VarInt) {
			if value.into_inner() == 0 {
				return;
			}
			buf.write_var(id);
			buf.write_var(value.size() as u64);
			buf.write(value);
		}

		let tp = &self.tp;
		varint(buf, MAX_IDLE_TIMEOUT, tp.max_idle_timeout);
		varint(buf, INITIAL_MAX_DATA, tp.initial_max_data);
		varint(
			buf,
			INITIAL_MAX_STREAM_DATA_BIDI_LOCAL,
			tp.initial_max_stream_data_bidi_local,
		);
		varint(
			buf,
			INITIAL_MAX_STREAM_DATA_BIDI_REMOTE,
			tp.initial_max_stream_data_bidi_remote,
		);
		varint(buf, INITIAL_MAX_STREAM_DATA_UNI, tp.initial_max_stream_data_uni);
		varint(buf, INITIAL_MAX_STREAMS_BIDI, tp.initial_max_streams_bidi);
		varint(buf, INITIAL_MAX_STREAMS_UNI, tp.initial_max_streams_uni);
		if let Some(size) = tp.max_datagram_frame_size {
			varint(buf, MAX_DATAGRAM_FRAME_SIZE, size);
		}
		varint(buf, MAX_RECORD_SIZE, self.max_record_size);
		if tp.reset_stream_at {
			buf.write_var(RESET_STREAM_AT);
			buf.write_var(0);
		}
		if let Some(protocols) = self.protocols.as_ref().filter(|p| !p.is_empty()) {
			let mut value = Vec::new();
			for protocol in protocols {
				value.write_var(protocol.len() as u64);
				value.put_slice(protocol.as_bytes());
			}
			buf.write_var(APPLICATION_PROTOCOLS);
			buf.write_var(value.len() as u64);
			buf.put_slice(&value);
		}
	}

	/// Decode a peer's parameters; `side` is the peer's
	pub(super) fn decode(side: Side, version: Version, blob: Bytes) -> Result<Self, TransportError> {
		let mut max_record_size = VarInt::from_u64(DEFAULT_MAX_RECORD_SIZE).unwrap();
		let mut protocols = None;
		let mut seen = HashSet::new();
		let mut scan = &blob[..];
		while scan.has_remaining() {
			let id = crate::coding::BufExt::get_var(&mut scan).map_err(|_| err("truncated parameter id"))?;
			let len = crate::coding::BufExt::get_var(&mut scan).map_err(|_| err("truncated parameter length"))?;
			let len = usize::try_from(len).map_err(|_| err("oversized parameter"))?;
			if scan.remaining() < len {
				return Err(err("truncated parameter value"));
			}
			let mut value = &scan[..len];
			scan.advance(len);

			if is_forbidden(id) {
				return Err(err("prohibited QUIC v1 transport parameter"));
			}
			if !seen.insert(id) {
				return Err(err("duplicate transport parameter"));
			}
			match id {
				// Draft-00 has no records, so its peer's value would only shrink our frames
				MAX_RECORD_SIZE if version.uses_records() => {
					max_record_size =
						crate::coding::BufExt::get(&mut value).map_err(|_| err("malformed max_record_size"))?;
					if value.has_remaining() {
						return Err(err("malformed max_record_size"));
					}
				}
				APPLICATION_PROTOCOLS => {
					let mut list = Vec::new();
					while value.has_remaining() {
						let len = crate::coding::BufExt::get_var(&mut value)
							.map_err(|_| err("malformed application_protocols"))?;
						let len = usize::try_from(len)
							.ok()
							.filter(|&len| len <= value.remaining())
							.ok_or_else(|| err("malformed application_protocols"))?;
						let name =
							std::str::from_utf8(&value[..len]).map_err(|_| err("application protocol is not UTF-8"))?;
						list.push(name.to_owned());
						value.advance(len);
					}
					protocols = Some(list);
				}
				_ => {}
			}
		}
		if version.uses_records() && max_record_size.into_inner() < DEFAULT_MAX_RECORD_SIZE {
			return Err(err("max_record_size below the minimum"));
		}

		let tp = TransportParameters::read(side, &mut &blob[..])?;
		Ok(Self {
			tp,
			max_record_size,
			protocols,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write_param(buf: &mut Vec<u8>, id: u64, value: u64) {
		let value = VarInt::from_u64(value).unwrap();
		buf.write_var(id);
		buf.write_var(value.size() as u64);
		buf.write(value);
	}

	fn decode(buf: Vec<u8>) -> Result<Params, TransportError> {
		Params::decode(Side::Client, Version::QMux02, buf.into())
	}

	#[test]
	fn round_trip() {
		let mut tp = TransportParameters::default();
		tp.max_idle_timeout = VarInt::from_u32(30_000);
		tp.initial_max_data = VarInt::from_u32(1 << 20);
		tp.initial_max_stream_data_uni = VarInt::from_u32(1 << 16);
		tp.initial_max_streams_uni = VarInt::from_u32(16);
		tp.max_datagram_frame_size = Some(VarInt::from_u32(4096));
		tp.reset_stream_at = true;
		let params = Params {
			tp,
			max_record_size: VarInt::from_u32(32_768),
			protocols: Some(vec!["moq-lite-03".into(), "moqt-16".into()]),
		};
		let mut buf = Vec::new();
		params.encode(&mut buf);
		let decoded = decode(buf).unwrap();
		assert_eq!(decoded.tp, params.tp);
		assert_eq!(decoded.max_record_size, params.max_record_size);
		assert_eq!(decoded.protocols, params.protocols);
	}

	#[test]
	fn rejects_prohibited() {
		// stateless_reset_token
		assert!(decode(vec![0x02, 0x00]).is_err());
	}

	#[test]
	fn rejects_duplicates_the_shared_codec_skips() {
		let mut buf = Vec::new();
		write_param(&mut buf, 0x1234, 7);
		write_param(&mut buf, 0x1234, 7);
		assert!(decode(buf).is_err());
	}

	#[test]
	fn ignores_unknown() {
		let mut buf = Vec::new();
		write_param(&mut buf, 0x1234, 7);
		write_param(&mut buf, INITIAL_MAX_DATA, 42);
		assert_eq!(decode(buf).unwrap().tp.initial_max_data, VarInt::from_u32(42));
	}

	#[test]
	fn record_size_is_for_records_only() {
		let mut buf = Vec::new();
		write_param(&mut buf, MAX_RECORD_SIZE, 1);
		assert!(decode(buf.clone()).is_err());
		let params = Params::decode(Side::Client, Version::QMux00, buf.into()).unwrap();
		assert_eq!(params.max_record_size.into_inner(), DEFAULT_MAX_RECORD_SIZE);
	}

	#[test]
	fn rejects_unrepresentable_stream_limit() {
		let mut buf = Vec::new();
		write_param(&mut buf, INITIAL_MAX_STREAMS_UNI, 1 << 61);
		assert!(decode(buf).is_err());
	}
}

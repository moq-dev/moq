//! What QMux adds to the QUIC v1 wire format
//!
//! QMux reuses the QUIC frame formats verbatim, so records are parsed with the crate's own
//! [`frame::Iter`]. This module supplies only what that parser does not know, the
//! `QX_TRANSPORT_PARAMETERS` and `QX_PING` frames, intercepted by peeking each frame's type
//! before the shared parser sees it.

use bytes::{Buf, BufMut, Bytes};

use crate::{
	TransportError, VarInt,
	coding::{BufExt, BufMutExt},
	frame,
};

/// Encodes as `\xffQMX\r\n\r\n`, which no HTTP/1.1 or HTTP/2 preface starts with
const QX_TRANSPORT_PARAMETERS: u64 = 0x3f5153300d0a0d0a;
const QX_PING_REQUEST: u64 = 0x348c67529ef8c7bd;
const QX_PING_RESPONSE: u64 = 0x348c67529ef8c7be;

/// The largest frame draft-00 accepts, header included, and the send-side ceiling for every
/// version: released peers reject a larger STREAM frame on receive.
pub(super) const MAX_FRAME_SIZE: usize = 16384;

/// The QMux wire format, chosen out of band by ALPN or the WebSocket subprotocol
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Version {
	/// draft-ietf-quic-qmux-00: bare frames, without records, QX_PING, or datagrams
	QMux00,
	/// draft-ietf-quic-qmux-01: frames in size-prefixed records, with QX_PING keep-alives,
	/// datagrams, and the `max_idle_timeout` and `max_record_size` parameters
	QMux01,
	/// draft-ietf-quic-qmux-02: draft-01 plus RESET_STREAM_AT, and stricter rules for
	/// QX_TRANSPORT_PARAMETERS and QX_PING
	QMux02,
}

impl Version {
	/// Every supported version, newest first
	pub const ALL: &[Self] = &[Self::QMux02, Self::QMux01, Self::QMux00];

	/// Whether frames travel in records, which brings QX_PING, datagrams, the idle timeout, and
	/// `max_record_size` along
	pub fn uses_records(self) -> bool {
		matches!(self, Self::QMux01 | Self::QMux02)
	}

	/// The bare ALPN for this version, like `qmux-01`
	pub fn alpn(self) -> &'static str {
		match self {
			Self::QMux00 => "qmux-00",
			Self::QMux01 => "qmux-01",
			Self::QMux02 => "qmux-02",
		}
	}

	/// The prefix pairing this version with an application protocol, like `qmux-01.`
	pub fn prefix(self) -> &'static str {
		match self {
			Self::QMux00 => "qmux-00.",
			Self::QMux01 => "qmux-01.",
			Self::QMux02 => "qmux-02.",
		}
	}
}

/// One frame of a record
#[derive(Debug)]
pub(super) enum Frame {
	/// A QUIC frame, including the ones QMux prohibits, which the connection rejects
	Quic(frame::Frame),
	/// A STREAM frame without the OFF bit, so it continues the stream: senders released
	/// before offsets were encoded omit it on every frame
	Continue(frame::Stream),
	TransportParameters(Bytes),
	PingRequest(VarInt),
	PingResponse(VarInt),
}

fn truncated() -> TransportError {
	TransportError::FRAME_ENCODING_ERROR("truncated frame")
}

/// Decode the next frame of a record, or `None` once it is exhausted
pub(super) fn next_frame(bytes: &mut Bytes) -> Result<Option<Frame>, TransportError> {
	if bytes.is_empty() {
		return Ok(None);
	}

	let mut peek = &bytes[..];
	let ty = peek.get_var().map_err(|_| truncated())?;
	let frame = match ty {
		QX_TRANSPORT_PARAMETERS | QX_PING_REQUEST | QX_PING_RESPONSE => {
			bytes.advance(bytes.len() - peek.len());
			match ty {
				QX_TRANSPORT_PARAMETERS => {
					let len = bytes.get_var().map_err(|_| truncated())?;
					let len = usize::try_from(len).map_err(|_| truncated())?;
					if bytes.remaining() < len {
						return Err(truncated());
					}
					Frame::TransportParameters(bytes.split_to(len))
				}
				QX_PING_REQUEST => Frame::PingRequest(bytes.get().map_err(|_| truncated())?),
				_ => Frame::PingResponse(bytes.get().map_err(|_| truncated())?),
			}
		}
		_ => {
			let mut iter = frame::Iter::new(std::mem::take(bytes))?;
			let frame = iter
				.next()
				.expect("payload is not empty")
				.map_err(TransportError::from)?;
			*bytes = iter.into_rest();
			match frame {
				frame::Frame::Stream(stream) if (0x08..=0x0f).contains(&ty) && ty & 0x04 == 0 => {
					Frame::Continue(stream)
				}
				frame => Frame::Quic(frame),
			}
		}
	};
	Ok(Some(frame))
}

pub(super) fn encode_transport_parameters(buf: &mut Vec<u8>, params: &[u8]) {
	buf.write_var(QX_TRANSPORT_PARAMETERS);
	buf.write_var(params.len() as u64);
	buf.put_slice(params);
}

pub(super) fn encode_ping(buf: &mut Vec<u8>, response: bool, seq: VarInt) {
	buf.write_var(match response {
		true => QX_PING_RESPONSE,
		false => QX_PING_REQUEST,
	});
	buf.write(seq);
}

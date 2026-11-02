use std::string::FromUtf8Error;
use thiserror::Error;

use super::{BoundsExceeded, Form, varint};

/// Read the value from a [`Decoder`] using the given version.
///
/// If [DecodeError::Short] is returned, the caller should try again with more data.
/// The value may borrow from the buffer for `'a`, so only a caller that keeps a field
/// copies it; an owned value implements this for every `'a`.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub trait Decode<'a, V>: Sized {
	/// Decode the value from the front of the decoder.
	fn decode(r: &mut Decoder<'a>, version: V) -> Result<Self, DecodeError>;

	/// Decode the value from the front of `buf`, returning it and the bytes it took.
	fn decode_slice(buf: &'a [u8], version: V) -> Result<(Self, usize), DecodeError>
	where
		V: Into<Form> + Copy,
	{
		let mut r = Decoder::new(buf, version.into());
		let value = Self::decode(&mut r, version)?;
		Ok((value, buf.len() - r.remaining()))
	}
}

/// A decode error.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum DecodeError {
	/// The buffer ran out mid-value. Retry once more bytes arrive.
	#[error("short buffer")]
	Short,

	/// The value claims more bytes than the enclosing message allows.
	#[error("long buffer")]
	Long,

	/// A string field was not valid UTF-8.
	#[error("invalid string")]
	InvalidString(#[from] FromUtf8Error),

	/// The message type ID is unknown for the negotiated version.
	#[error("invalid message: {0:?}")]
	InvalidMessage(u64),

	/// A SUBSCRIBE start/end location is malformed or out of order.
	#[error("invalid subscribe location")]
	InvalidSubscribeLocation,

	/// A field held a value outside its permitted range.
	#[error("invalid value")]
	InvalidValue,

	/// A repeated field exceeded the count this implementation accepts.
	#[error("too many")]
	TooMany,

	/// An integer was too large for the field it was read into.
	#[error("bounds exceeded")]
	BoundsExceeded,

	/// More data followed where the message was required to end.
	#[error("expected end")]
	ExpectedEnd,

	/// A length-prefixed message exceeded the receiver's byte limit.
	#[error("message too large: {size} bytes exceeds {max} byte limit")]
	MessageTooLarge {
		/// The byte length declared by the peer.
		size: usize,
		/// The largest message this receiver accepts.
		max: usize,
	},

	/// A parameter or field appeared more than once.
	#[error("duplicate")]
	Duplicate,

	/// Bytes remained after the value was fully decoded.
	#[error("trailing bytes")]
	TrailingBytes,

	/// The field does not exist in the negotiated protocol version.
	#[error("unsupported version")]
	Version,

	/// The value is one the draft answers by closing the session with this code, such as an
	/// `AUTHORIZATION TOKEN` naming an alias.
	#[error(transparent)]
	Session(crate::SessionError),

	/// A request's `AUTHORIZATION TOKEN` is well formed but unusable, such as a repeat of
	/// another, which rejects the request rather than closing the session.
	#[error("malformed authorization token")]
	MalformedAuthToken,

	/// A request carries more than one distinct `AUTHORIZATION TOKEN`, which this
	/// implementation refuses as unsupported rather than closing the session.
	#[error("multiple authorization tokens")]
	MultipleAuthTokens,
}

impl DecodeError {
	/// A complete frame cannot be extended by reading more stream bytes.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn complete(self) -> Self {
		match self {
			Self::Short => Self::InvalidValue,
			other => other,
		}
	}
}

impl From<BoundsExceeded> for DecodeError {
	fn from(_: BoundsExceeded) -> Self {
		Self::BoundsExceeded
	}
}

/// Reads wire primitives from the front of a byte slice.
///
/// A read either consumes exactly what it returns or fails and consumes nothing, so a
/// [`DecodeError::Short`] can be retried once more bytes arrive.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub struct Decoder<'a> {
	buf: &'a [u8],
	form: Form,
	/// The length of the buffer this decoder started with.
	len: usize,
	/// See [`Self::need`].
	need: usize,
	/// The control message read, counted once the read is consumed.
	pub(crate) control: Option<crate::stats::Message>,
	/// Leading bytes left out of the `control` count: a type selector the sender writes
	/// apart from the message, and so never counts.
	pub(crate) uncounted: usize,
}

impl<'a> Decoder<'a> {
	/// Read `buf`, with varints in the given form.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn new(buf: &'a [u8], form: Form) -> Self {
		Self {
			buf,
			form,
			len: buf.len(),
			need: 0,
			control: None,
			uncounted: 0,
		}
	}

	/// After a [`DecodeError::Short`], how many bytes from the start the failed read
	/// needed: at least one more than the buffer when it can't tell, and 0 if no read of
	/// this decoder ran short.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn need(&self) -> usize {
		self.need
	}

	/// Record that a read wanted `more` bytes past the current position.
	fn short(&mut self, more: usize) -> DecodeError {
		let need = (self.len - self.buf.len()).saturating_add(more);
		self.need = self.need.max(need);
		DecodeError::Short
	}

	/// The varint form this decoder reads.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn form(&self) -> Form {
		self.form
	}

	/// How many bytes have been read.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn position(&self) -> usize {
		self.len - self.buf.len()
	}

	/// The number of unread bytes.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn remaining(&self) -> usize {
		self.buf.len()
	}

	/// Whether every byte has been read.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn is_empty(&self) -> bool {
		self.buf.is_empty()
	}

	/// Read `len` raw bytes.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn slice(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
		let Some((head, rest)) = self.buf.split_at_checked(len) else {
			return Err(self.short(len));
		};
		self.buf = rest;
		Ok(head)
	}

	/// Read every remaining byte.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn rest(&mut self) -> &'a [u8] {
		std::mem::take(&mut self.buf)
	}

	/// Split off the next `len` bytes as their own decoder, e.g. a size-prefixed body.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn sub(&mut self, len: usize) -> Result<Self, DecodeError> {
		Ok(Self::new(self.slice(len)?, self.form))
	}

	/// Read a single byte.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn u8(&mut self) -> Result<u8, DecodeError> {
		Ok(self.slice(1)?[0])
	}

	/// Read a big-endian `u16`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn u16(&mut self) -> Result<u16, DecodeError> {
		let b = self.slice(2)?;
		Ok(u16::from_be_bytes([b[0], b[1]]))
	}

	/// Read a byte that must be 0 or 1.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn bool(&mut self) -> Result<bool, DecodeError> {
		match self.u8()? {
			0 => Ok(false),
			1 => Ok(true),
			_ => Err(DecodeError::InvalidValue),
		}
	}

	/// Read a varint.
	#[inline(always)]
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn varint(&mut self) -> Result<u64, DecodeError> {
		match varint::read(self.buf, self.form) {
			Ok((value, rest)) => {
				self.buf = rest;
				Ok(value)
			}
			// The width is in the first byte, but a byte at a time is short enough.
			Err(DecodeError::Short) => Err(self.short(self.buf.len() + 1)),
			Err(err) => Err(err),
		}
	}

	/// Read an optional varint: 0 is `None`, and `n + 1` is `Some(n)`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn varint_opt(&mut self) -> Result<Option<u64>, DecodeError> {
		Ok(self.varint()?.checked_sub(1))
	}

	/// Read a varint length, then that many raw bytes.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
		let start = self.buf;
		let len = usize::try_from(self.varint()?).map_err(|_| DecodeError::BoundsExceeded)?;
		self.slice(len).inspect_err(|_| self.buf = start)
	}

	/// Read a varint length, then that many bytes of UTF-8, borrowed from the buffer.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn str(&mut self) -> Result<&'a str, DecodeError> {
		let bytes = self.bytes()?;
		match std::str::from_utf8(bytes) {
			Ok(s) => Ok(s),
			// The error carries an owned copy, so only invalid input pays for one.
			Err(_) => Err(String::from_utf8(bytes.to_vec()).unwrap_err().into()),
		}
	}

	/// Read a varint length, then that many bytes of UTF-8.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn string(&mut self) -> Result<String, DecodeError> {
		Ok(self.str()?.to_owned())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A short read must leave the decoder where it was, or a retry with more bytes
	/// would start mid-value.
	#[test]
	fn short_consumes_nothing() {
		let mut r = Decoder::new(&[0x05, b'a', b'b'], Form::Quic);
		assert!(matches!(r.bytes(), Err(DecodeError::Short)));
		assert_eq!(r.remaining(), 3);

		let mut r = Decoder::new(&[0x40], Form::Quic);
		assert!(matches!(r.varint(), Err(DecodeError::Short)));
		assert_eq!(r.remaining(), 1);
	}

	/// A short read reports how much it needed from the start, so a reader can wait for
	/// all of it rather than retry once per byte.
	#[test]
	fn short_reports_what_it_needed() {
		let mut r = Decoder::new(&[0x01, 0x05, b'a', b'b'], Form::Quic);
		assert_eq!(r.varint().unwrap(), 1);
		assert!(matches!(r.bytes(), Err(DecodeError::Short)));
		assert_eq!(r.need(), 7, "a 5 byte body after two prefix bytes");

		let mut r = Decoder::new(&[0x40], Form::Quic);
		assert!(matches!(r.varint(), Err(DecodeError::Short)));
		assert_eq!(r.need(), 2);
	}
}

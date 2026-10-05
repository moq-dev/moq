use bytes::Bytes;

use super::{BoundsExceeded, Form, varint};

/// An error that occurs during encoding.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum EncodeError {
	/// An integer was too large for the QUIC varint range.
	#[error("bounds exceeded")]
	BoundsExceeded,
	/// The payload exceeds the maximum size the wire format can express.
	#[error("too large")]
	TooLarge,
	/// The destination buffer had no room for the value.
	#[error("short buffer")]
	Short,
	/// The message cannot be encoded from the current session state.
	#[error("invalid state")]
	InvalidState,
	/// A repeated field exceeded the count the wire format permits.
	#[error("too many")]
	TooMany,
	/// The field does not exist in the negotiated protocol version.
	#[error("unsupported version")]
	Version,
	/// The value is well-formed but this implementation cannot put it on the wire.
	#[error("unsupported")]
	Unsupported,
}

impl From<BoundsExceeded> for EncodeError {
	fn from(_: BoundsExceeded) -> Self {
		Self::BoundsExceeded
	}
}

/// Write the value to an [`Encoder`] using the given version.
pub trait Encode<V> {
	/// Encode the value to the given encoder.
	fn encode(&self, w: &mut Encoder<'_>, version: V) -> Result<(), EncodeError>;

	/// Encode the value into a fresh [Bytes] buffer.
	fn encode_bytes(&self, version: V) -> Result<Bytes, EncodeError>
	where
		V: Into<Form> + Copy,
	{
		let mut buf = Vec::new();
		self.encode(&mut Encoder::new(&mut buf, version.into()), version)?;
		Ok(buf.into())
	}
}

/// Appends wire primitives to a byte buffer.
///
/// The buffer grows as needed, so only a value the wire cannot express fails.
#[derive(Debug)]
pub struct Encoder<'a> {
	buf: &'a mut Vec<u8>,
	form: Form,
}

impl<'a> Encoder<'a> {
	/// Append to `buf`, with varints in the given form.
	pub fn new(buf: &'a mut Vec<u8>, form: Form) -> Self {
		Self { buf, form }
	}

	/// The varint form this encoder writes.
	pub fn form(&self) -> Form {
		self.form
	}

	/// Write raw bytes.
	pub fn slice(&mut self, v: &[u8]) {
		self.buf.extend_from_slice(v);
	}

	/// Write a single byte.
	pub fn u8(&mut self, v: u8) {
		self.buf.push(v);
	}

	/// Write a big-endian `u16`.
	pub fn u16(&mut self, v: u16) {
		self.buf.extend_from_slice(&v.to_be_bytes());
	}

	/// Write a bool as a 0 or 1 byte.
	pub fn bool(&mut self, v: bool) {
		self.buf.push(v as u8);
	}

	/// Write a varint, or fail with [`EncodeError::BoundsExceeded`] if the form cannot carry it.
	#[cfg_attr(target_arch = "wasm32", inline)]
	#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
	pub fn varint(&mut self, v: u64) -> Result<(), EncodeError> {
		Ok(varint::write(v, self.form, self.buf)?)
	}

	/// Write an optional varint: `None` as 0, and `Some(n)` as `n + 1`.
	pub fn varint_opt(&mut self, v: Option<u64>) -> Result<(), EncodeError> {
		let v = match v {
			Some(v) => v.checked_add(1).ok_or(EncodeError::TooLarge)?,
			None => 0,
		};
		self.varint(v)
	}

	/// Write a varint length, then the raw bytes.
	pub fn bytes(&mut self, v: &[u8]) -> Result<(), EncodeError> {
		self.varint(v.len() as u64)?;
		self.slice(v);
		Ok(())
	}

	/// Write a varint length, then the UTF-8 bytes.
	pub fn string(&mut self, v: &str) -> Result<(), EncodeError> {
		self.bytes(v.as_bytes())
	}

	/// Reserve a varint size prefix for the body written next; [`Self::fill`] sizes it.
	///
	/// One byte is reserved, which most bodies fit, so they never move.
	pub fn prefix_varint(&mut self) -> Prefix {
		self.buf.push(0);
		Prefix {
			body: self.buf.len(),
			kind: PrefixKind::Varint,
		}
	}

	/// Reserve a big-endian `u16` size prefix for the body written next; [`Self::fill`] sizes it.
	pub fn prefix_u16(&mut self) -> Prefix {
		self.buf.extend_from_slice(&[0, 0]);
		Prefix {
			body: self.buf.len(),
			kind: PrefixKind::U16,
		}
	}

	/// The number of bytes written since `prefix` was reserved.
	pub fn since(&self, prefix: &Prefix) -> usize {
		self.buf.len() - prefix.body
	}

	/// Drop a reserved prefix and everything written since, as if neither was written.
	pub fn discard(&mut self, prefix: Prefix) {
		let start = match prefix.kind {
			PrefixKind::Varint => prefix.body - 1,
			PrefixKind::U16 => prefix.body - 2,
		};
		self.buf.truncate(start);
	}

	/// Size a reserved prefix to everything written since.
	pub fn fill(&mut self, prefix: Prefix) -> Result<(), EncodeError> {
		let body = prefix.body;
		let end = self.buf.len();

		match prefix.kind {
			PrefixKind::U16 => {
				let size = u16::try_from(end - body).map_err(|_| EncodeError::TooLarge)?;
				self.buf[body - 2..body].copy_from_slice(&size.to_be_bytes());
			}
			PrefixKind::Varint => {
				// Encode the size past the body, then move it into the reserved byte,
				// shifting the body up when the size needs more than that one byte.
				varint::write((end - body) as u64, self.form, self.buf)?;
				let len = self.buf.len() - end;
				if len == 1 {
					self.buf[body - 1] = self.buf[end];
					self.buf.truncate(end);
					return Ok(());
				}

				let mut size = [0u8; 9];
				size[..len].copy_from_slice(&self.buf[end..]);
				self.buf.truncate(end + len - 1);
				self.buf.copy_within(body..end, body + len - 1);
				self.buf[body - 1..body - 1 + len].copy_from_slice(&size[..len]);
			}
		}
		Ok(())
	}
}

/// A size prefix reserved ahead of a body, sized by [`Encoder::fill`] once it is written.
#[must_use = "an unfilled prefix leaves a zero size on the wire"]
#[derive(Debug)]
pub struct Prefix {
	/// Where the body starts, just past the reserved bytes.
	body: usize,
	kind: PrefixKind,
}

#[derive(Debug)]
enum PrefixKind {
	Varint,
	U16,
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The prefix lands before the body, sized to it, even when it outgrows the one byte
	/// reserved for it.
	#[test]
	fn prefix_varint_sizes_the_body() {
		for size in [0usize, 63, 64, 20_000] {
			let mut buf = vec![0xaa];
			let mut w = Encoder::new(&mut buf, Form::Quic);
			let prefix = w.prefix_varint();
			w.slice(&vec![0x55; size]);
			w.fill(prefix).unwrap();
			w.u8(0xbb);

			let mut r = super::super::Decoder::new(&buf[1..], Form::Quic);
			assert_eq!(buf[0], 0xaa);
			assert_eq!(r.varint().unwrap(), size as u64);
			assert_eq!(r.slice(size).unwrap(), vec![0x55; size]);
			assert_eq!(r.rest(), [0xbb]);
		}
	}

	#[test]
	fn prefix_u16_sizes_the_body() {
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, Form::Quic);
		let prefix = w.prefix_u16();
		w.slice(&[0x55; 300]);
		w.fill(prefix).unwrap();
		assert_eq!(buf[..2], 300u16.to_be_bytes());
		assert_eq!(buf.len(), 302);

		let mut w = Encoder::new(&mut buf, Form::Quic);
		let prefix = w.prefix_u16();
		w.slice(&vec![0; 1 << 16]);
		assert!(matches!(w.fill(prefix), Err(EncodeError::TooLarge)));
	}
}

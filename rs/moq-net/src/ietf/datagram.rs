//! OBJECT_DATAGRAM: one Object carried in a QUIC datagram (draft-14 section 10.3.1 through
//! draft-20 section 11.3.1).
//!
//! The model counterpart is [`crate::Datagram`], a single-frame group, so only an Object at
//! ID 0 maps onto it. The Type is a set of flags on every draft; draft-14 lacks the
//! DEFAULT_PRIORITY bit and a status with an omitted Object ID.

use bytes::{Buf, BufMut, Bytes};

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use super::Version;

/// The bits of an OBJECT_DATAGRAM Type.
mod flag {
	pub const PROPERTIES: u64 = 0x01;
	pub const END_OF_GROUP: u64 = 0x02;
	pub const ZERO_OBJECT_ID: u64 = 0x04;
	pub const DEFAULT_PRIORITY: u64 = 0x08;
	pub const STATUS: u64 = 0x20;
	/// Every defined bit. Anything else, including the reserved 0x10, is invalid.
	pub const ALL: u64 = PROPERTIES | END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY | STATUS;
}

/// What follows an OBJECT_DATAGRAM's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatagramBody {
	/// The Object Payload, delimited by the datagram boundary.
	Payload(Bytes),
	/// The Object Status of an Object without a payload.
	Status(u64),
}

/// A decoded OBJECT_DATAGRAM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectDatagram {
	pub track_alias: u64,
	pub group_id: u64,
	/// The Object ID, or `None` when the ZERO_OBJECT_ID bit omits it (Object 0).
	pub object_id: Option<u64>,
	/// The Publisher Priority, or `None` to inherit the subscription's (draft-15+).
	pub publisher_priority: Option<u8>,
	/// No Object past this one exists in the group.
	pub end_of_group: bool,
	/// The Object Properties block without its length prefix, which carries the Timestamp.
	pub properties: Option<Vec<u8>>,
	pub body: DatagramBody,
}

impl ObjectDatagram {
	/// Whether `kind` is a Type this draft defines.
	fn valid(kind: u64, version: Version) -> bool {
		match version {
			Version::Draft14 => kind <= 0x07 || kind == 0x20 || kind == 0x21,
			// A status cannot also end the group.
			_ => kind & !flag::ALL == 0 && !(kind & flag::STATUS != 0 && kind & flag::END_OF_GROUP != 0),
		}
	}
}

impl Encode<Version> for ObjectDatagram {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let mut kind = 0;
		if self.properties.is_some() {
			kind |= flag::PROPERTIES;
		}
		if self.end_of_group {
			kind |= flag::END_OF_GROUP;
		}
		if self.object_id.is_none() {
			kind |= flag::ZERO_OBJECT_ID;
		}
		if self.publisher_priority.is_none() {
			kind |= flag::DEFAULT_PRIORITY;
		}
		if matches!(self.body, DatagramBody::Status(_)) {
			kind |= flag::STATUS;
		}
		if !Self::valid(kind, version) {
			return Err(EncodeError::InvalidState);
		}

		kind.encode(w, version)?;
		self.track_alias.encode(w, version)?;
		self.group_id.encode(w, version)?;
		if let Some(object_id) = self.object_id {
			object_id.encode(w, version)?;
		}
		if let Some(priority) = self.publisher_priority {
			priority.encode(w, version)?;
		}
		if let Some(properties) = &self.properties {
			// A present but empty block is a protocol violation for the peer.
			if properties.is_empty() {
				return Err(EncodeError::InvalidState);
			}
			properties.encode(w, version)?;
		}

		match &self.body {
			DatagramBody::Status(status) => status.encode(w, version)?,
			DatagramBody::Payload(payload) => {
				// Runs to the datagram boundary: written raw, no length prefix.
				if w.remaining_mut() < payload.len() {
					return Err(EncodeError::Short);
				}
				w.put_slice(payload);
			}
		}
		Ok(())
	}
}

impl Decode<Version> for ObjectDatagram {
	fn decode<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let kind = u64::decode(r, version)?;
		if !Self::valid(kind, version) {
			return Err(DecodeError::InvalidValue);
		}

		let track_alias = u64::decode(r, version)?;
		let group_id = u64::decode(r, version)?;
		let object_id = match kind & flag::ZERO_OBJECT_ID != 0 {
			true => None,
			false => Some(u64::decode(r, version)?),
		};
		let publisher_priority = match kind & flag::DEFAULT_PRIORITY != 0 {
			true => None,
			false => Some(u8::decode(r, version)?),
		};
		let properties = match kind & flag::PROPERTIES != 0 {
			true => {
				let properties = Vec::<u8>::decode(r, version)?;
				if properties.is_empty() {
					return Err(DecodeError::InvalidValue);
				}
				Some(properties)
			}
			false => None,
		};

		let body = match kind & flag::STATUS != 0 {
			true => {
				let status = u64::decode(r, version)?;
				// Draft-17 on: only a Normal Object may carry Properties.
				let legacy = matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16);
				if !legacy && status != 0 && properties.is_some() {
					return Err(DecodeError::InvalidValue);
				}
				if r.has_remaining() {
					return Err(DecodeError::TrailingBytes);
				}
				DatagramBody::Status(status)
			}
			false => DatagramBody::Payload(r.copy_to_bytes(r.remaining())),
		};

		Ok(Self {
			track_alias,
			group_id,
			object_id,
			publisher_priority,
			end_of_group: kind & flag::END_OF_GROUP != 0,
			properties,
			body,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const ALL: [Version; 9] = [
		Version::Draft14,
		Version::Draft15,
		Version::Draft16,
		Version::Draft17,
		Version::Draft18,
		Version::Draft19,
		Version::Draft20,
		Version::Draft21,
		Version::Draft22,
	];

	fn single(priority: Option<u8>) -> ObjectDatagram {
		ObjectDatagram {
			track_alias: 3,
			group_id: 42,
			object_id: None,
			publisher_priority: priority,
			end_of_group: true,
			properties: Some(vec![0x10, 0x05]),
			body: DatagramBody::Payload(Bytes::from_static(b"hello")),
		}
	}

	#[test]
	fn roundtrip_every_draft() {
		for version in ALL {
			let datagram = single(Some(7));
			let mut buf = datagram.encode_bytes(version).unwrap();
			assert_eq!(buf[0], 0x07, "{version}: properties, end of group, object 0");
			let decoded = ObjectDatagram::decode(&mut buf, version).unwrap();
			assert_eq!(decoded, datagram, "{version}");
			assert!(!buf.has_remaining());
		}
	}

	#[test]
	fn default_priority_needs_draft15() {
		assert!(matches!(
			single(None).encode_bytes(Version::Draft14),
			Err(EncodeError::InvalidState)
		));
		assert!(ObjectDatagram::decode(&mut &[0x0C, 0x01, 0x02][..], Version::Draft14).is_err());

		let mut buf = single(None).encode_bytes(Version::Draft16).unwrap();
		assert_eq!(buf[0], 0x0F);
		assert_eq!(
			ObjectDatagram::decode(&mut buf, Version::Draft16).unwrap(),
			single(None)
		);
	}

	#[test]
	fn explicit_object_id_and_status() {
		let datagram = ObjectDatagram {
			track_alias: 1,
			group_id: 2,
			object_id: Some(0),
			publisher_priority: Some(128),
			end_of_group: false,
			properties: None,
			body: DatagramBody::Status(0),
		};
		for version in ALL {
			let mut buf = datagram.encode_bytes(version).unwrap();
			assert_eq!(buf[0], 0x20, "{version}");
			assert_eq!(ObjectDatagram::decode(&mut buf, version).unwrap(), datagram);
		}
	}

	#[test]
	fn rejects_invalid_types() {
		for version in ALL {
			// A status that ends the group, the reserved bit, and a bit past the defined ones.
			for kind in [0x22u64, 0x10, 0x40] {
				let mut bytes = kind.encode_bytes(version).unwrap().to_vec();
				bytes.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
				assert!(
					ObjectDatagram::decode(&mut &bytes[..], version).is_err(),
					"{version}: {kind:#x}"
				);
			}
		}
	}

	#[test]
	fn rejects_empty_properties() {
		// PROPERTIES and ZERO_OBJECT_ID, alias 1, group 2, priority 0, empty block.
		let bytes = [0x05, 0x01, 0x02, 0x00, 0x00, b'x'];
		assert!(matches!(
			ObjectDatagram::decode(&mut &bytes[..], Version::Draft16),
			Err(DecodeError::InvalidValue)
		));
	}
}

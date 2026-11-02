//! OBJECT_DATAGRAM: one Object carried in a QUIC datagram (draft-22 section 11.2.1).
//!
//! Public [`crate::Datagram`] writers emit Object ID 0; private relay metadata preserves
//! other IDs, properties, and status. The Type is a set of flags.

use bytes::Bytes;

use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

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
	/// The Publisher Priority, or `None` to inherit the subscription's.
	pub publisher_priority: Option<u8>,
	/// No Object past this one exists in the group.
	pub end_of_group: bool,
	/// The Object Properties block without its length prefix, which carries the Timestamp.
	pub properties: Option<Vec<u8>>,
	pub body: DatagramBody,
}

impl ObjectDatagram {
	/// Whether `kind` is a defined Type. A status cannot also end the group.
	fn valid(kind: u64) -> bool {
		kind & !flag::ALL == 0 && !(kind & flag::STATUS != 0 && kind & flag::END_OF_GROUP != 0)
	}

	/// The Type bits this datagram's fields set.
	fn kind(&self) -> u64 {
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
		kind
	}
}

impl crate::model::datagram::Wire {
	/// Whether a datagram of this shape is defined, as ingress checks one. Egress resolves
	/// the Object ID and priority each draft needs.
	pub(crate) fn defined(&self) -> bool {
		let shape = ObjectDatagram {
			track_alias: 0,
			group_id: 0,
			object_id: self.object_id,
			publisher_priority: self.priority,
			end_of_group: self.end_of_group,
			properties: self.properties.block.is_some().then(Vec::new),
			body: match self.status {
				Some(status) => DatagramBody::Status(status),
				None => DatagramBody::Payload(Bytes::new()),
			},
		};
		ObjectDatagram::valid(shape.kind())
			&& self.status.is_none_or(|status| {
				super::object::status_defined(status) && (self.properties.block.is_none() || status == 0)
			})
	}
}

impl Encode<Version> for ObjectDatagram {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		let kind = self.kind();
		if !Self::valid(kind) {
			return Err(EncodeError::InvalidState);
		}

		w.varint(kind)?;
		w.varint(self.track_alias)?;
		w.varint(self.group_id)?;
		if let Some(object_id) = self.object_id {
			w.varint(object_id)?;
		}
		if let Some(priority) = self.publisher_priority {
			w.u8(priority);
		}
		if let Some(properties) = &self.properties {
			// A present but empty block is a protocol violation for the peer.
			if properties.is_empty() {
				return Err(EncodeError::InvalidState);
			}
			w.bytes(properties)?;
		}

		match &self.body {
			DatagramBody::Status(status) => {
				// Only a Normal Object may carry properties.
				if self.properties.is_some() && *status != 0 {
					return Err(EncodeError::InvalidState);
				}
				w.varint(*status)?
			}
			DatagramBody::Payload(payload) => {
				// Runs to the datagram boundary: written raw, no length prefix.
				w.slice(payload);
			}
		}
		Ok(())
	}
}

impl<'a> Decode<'a, Version> for ObjectDatagram {
	fn decode(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let kind = r.varint()?;
		if !Self::valid(kind) {
			return Err(DecodeError::InvalidValue);
		}

		let track_alias = r.varint()?;
		let group_id = r.varint()?;
		let object_id = match kind & flag::ZERO_OBJECT_ID != 0 {
			true => None,
			false => Some(r.varint()?),
		};
		let publisher_priority = match kind & flag::DEFAULT_PRIORITY != 0 {
			true => None,
			false => Some(r.u8()?),
		};
		let properties = match kind & flag::PROPERTIES != 0 {
			true => {
				let properties = r.bytes()?.to_vec();
				if properties.is_empty() {
					return Err(DecodeError::InvalidValue);
				}
				Some(properties)
			}
			false => None,
		};

		let body = match kind & flag::STATUS != 0 {
			true => {
				let status = r.varint()?;
				if properties.is_some() && status != 0 {
					return Err(DecodeError::InvalidValue);
				}
				if !r.is_empty() {
					return Err(DecodeError::TrailingBytes);
				}
				DatagramBody::Status(status)
			}
			false => DatagramBody::Payload(Bytes::copy_from_slice(r.rest())),
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

	fn decode(buf: &[u8], version: Version) -> Result<ObjectDatagram, DecodeError> {
		let (datagram, used) = ObjectDatagram::decode_slice(buf, version)?;
		assert_eq!(used, buf.len(), "a datagram runs to its end");
		Ok(datagram)
	}

	const ALL: [Version; 3] = [Version::Draft18, Version::Draft20, Version::Draft22];

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
			let buf = datagram.encode_bytes(version).unwrap();
			assert_eq!(buf[0], 0x07, "{version}: properties, end of group, object 0");
			assert_eq!(decode(&buf, version).unwrap(), datagram, "{version}");
		}
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
			let buf = datagram.encode_bytes(version).unwrap();
			assert_eq!(buf[0], 0x20, "{version}");
			assert_eq!(decode(&buf, version).unwrap(), datagram);
		}
	}

	#[test]
	fn rejects_invalid_types() {
		for version in ALL {
			// A status that ends the group, the reserved bit, and a bit past the defined ones.
			for kind in [0x22u64, 0x10, 0x40] {
				let mut bytes = Vec::new();
				let mut w = Encoder::new(&mut bytes, version.into());
				w.varint(kind).unwrap();
				w.slice(&[0x01, 0x02, 0x03, 0x04]);
				assert!(
					ObjectDatagram::decode_slice(&bytes, version).is_err(),
					"{version}: {kind:#x}"
				);
			}
		}
	}

	#[test]
	fn status_with_properties_needs_normal() {
		let datagram = ObjectDatagram {
			track_alias: 1,
			group_id: 2,
			object_id: Some(0),
			publisher_priority: Some(0),
			end_of_group: false,
			properties: Some(vec![0x10, 0x05]),
			body: DatagramBody::Status(3),
		};
		for version in ALL {
			let err = datagram.encode_bytes(version).unwrap_err();
			assert!(matches!(err, EncodeError::InvalidState), "{version}");
		}
	}

	#[test]
	fn rejects_empty_properties() {
		// PROPERTIES and ZERO_OBJECT_ID, alias 1, group 2, priority 0, empty block.
		let bytes = [0x05, 0x01, 0x02, 0x00, 0x00, b'x'];
		assert!(matches!(
			ObjectDatagram::decode_slice(&bytes, Version::Draft18),
			Err(DecodeError::InvalidValue)
		));
	}
}

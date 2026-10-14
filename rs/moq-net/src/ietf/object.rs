use bytes::Bytes;

use super::Version;
use crate::{
	Error, Timescale, Timestamp,
	coding::{Decoder, Encoder},
	model::object::{Encoding, Properties, Property, Value},
};

pub(super) fn encoding(version: Version) -> Encoding {
	match version {
		Version::Draft14 | Version::Draft15 => Encoding::Absolute,
		Version::Draft16 => Encoding::DeltaQuic,
		Version::Draft17 => Encoding::Delta { seven: false },
		_ => Encoding::Delta { seven: true },
	}
}

pub(super) struct Decoded {
	pub properties: Properties,
	pub timestamp: Option<Timestamp>,
	pub gap: Option<u64>,
}

struct List {
	properties: Properties,
	time: Option<u64>,
	gap: Option<u64>,
}

fn list(data: &Bytes, version: Version, immutable: bool) -> Result<List, Error> {
	let keys = encoding(version);
	let mut decoder = Decoder::new(data, version.into());
	let mut result = List {
		properties: Properties::default(),
		time: None,
		gap: None,
	};
	let mut previous = 0u64;
	while !decoder.is_empty() {
		let key = decoder.varint()?;
		let id = match keys {
			Encoding::Absolute => key,
			Encoding::Delta { .. } | Encoding::DeltaQuic => previous.checked_add(key).ok_or(Error::MalformedTrack)?,
		};
		previous = id;
		let value = if id % 2 == 0 {
			let value = decoder.varint()?;
			if matches!(id, 0x10 | 0x06) {
				result.time = Some(value);
			}
			if id == 0x3e && result.gap.replace(value).is_some() {
				return Err(Error::MalformedTrack);
			}
			if id == 0x10 {
				if result.properties.timestamp.replace(value).is_some() {
					return Err(Error::MalformedTrack);
				}
				continue;
			}
			Value::Int(value)
		} else {
			let bytes = decoder.bytes()?;
			if bytes.len() > u16::MAX as usize {
				return Err(Error::MalformedTrack);
			}
			// A small cached property must not pin the reader's larger receive buffer.
			let bytes = Bytes::copy_from_slice(bytes);
			if id == 0x0b {
				if immutable || result.properties.immutable.replace(keys).is_some() {
					return Err(Error::MalformedTrack);
				}
				let nested = list(&bytes, version, true).map_err(|_| Error::MalformedTrack)?;
				result.time = result.time.or(nested.time);
				if let Some(gap) = nested.gap
					&& result.gap.replace(gap).is_some()
				{
					return Err(Error::MalformedTrack);
				}
			}
			Value::Bytes(bytes)
		};
		result.properties.values.push(Property { id, value });
	}
	Ok(result)
}

pub(super) fn read(data: &Bytes, version: Version, timescale: Option<Timescale>) -> Result<Decoded, Error> {
	let parsed = list(data, version, false)?;
	let timestamp = match (parsed.time, timescale) {
		(Some(value), Some(timescale)) => Some(Timestamp::new(value, timescale).map_err(|_| Error::MalformedTrack)?),
		_ => None,
	};
	Ok(Decoded {
		properties: parsed.properties,
		timestamp,
		gap: parsed.gap,
	})
}

pub(super) fn write(properties: &Properties, version: Version) -> Result<Vec<u8>, Error> {
	if properties
		.immutable
		.is_some_and(|original| original != encoding(version))
	{
		return Err(Error::Unsupported);
	}
	let timestamp = properties.timestamp.map(|value| Property {
		id: 0x10,
		value: Value::Int(value),
	});
	let mut sorted: Vec<_> = properties.values.iter().chain(timestamp.as_ref()).collect();
	// Delta keys require ascending order. Equal keys retain the publisher's order.
	sorted.sort_by_key(|property| property.id);
	let mut out = Vec::new();
	let mut encoder = Encoder::new(&mut out, version.into());
	let mut previous = 0;
	for property in sorted {
		encoder.varint(match encoding(version) {
			Encoding::Absolute => property.id,
			Encoding::Delta { .. } | Encoding::DeltaQuic => property.id - previous,
		})?;
		previous = property.id;
		match &property.value {
			Value::Int(value) => encoder.varint(*value)?,
			Value::Bytes(value) => encoder.bytes(value)?,
		}
	}
	Ok(out)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cached_bytes_do_not_retain_the_receive_buffer() {
		use std::sync::{
			Arc,
			atomic::{AtomicBool, Ordering},
		};
		struct Buffer {
			bytes: Vec<u8>,
			dropped: Arc<AtomicBool>,
		}
		impl AsRef<[u8]> for Buffer {
			fn as_ref(&self) -> &[u8] {
				&self.bytes
			}
		}
		impl Drop for Buffer {
			fn drop(&mut self) {
				self.dropped.store(true, Ordering::Relaxed);
			}
		}
		let dropped = Arc::new(AtomicBool::new(false));
		let mut bytes = vec![0; 65536];
		bytes[..3].copy_from_slice(&[5, 1, 42]);
		let received = Bytes::from_owner(Buffer {
			bytes,
			dropped: dropped.clone(),
		})
		.slice(..3);
		let decoded = read(&received, Version::Draft22, None).unwrap();
		drop(received);
		assert!(dropped.load(Ordering::Relaxed));
		assert_eq!(
			decoded.properties.values[0].value,
			Value::Bytes(Bytes::from_static(&[42]))
		);
	}

	#[test]
	fn immutable_timestamp_is_read_without_adding_an_outer_property() {
		let bytes = Bytes::from_static(&[0x0b, 2, 0x10, 42]);
		let decoded = read(&bytes, Version::Draft22, Some(Timescale::MILLI)).unwrap();
		assert_eq!(decoded.timestamp, Some(Timestamp::from_millis(42).unwrap()));
		assert_eq!(decoded.properties.timestamp, None);
		assert_eq!(write(&decoded.properties, Version::Draft18).unwrap(), bytes);
		assert!(matches!(
			write(&decoded.properties, Version::Draft17),
			Err(Error::Unsupported)
		));
	}

	#[test]
	fn immutable_lists_validate_nested_blocks_and_integer_forms() {
		for bytes in [&[0x0b, 2, 0x0b, 0][..], &[0x0b, 0, 0, 0][..], &[0x0b, 1, 2][..]] {
			assert!(read(&Bytes::copy_from_slice(bytes), Version::Draft22, None).is_err());
		}
		// Draft-18 permits this nonminimal seven-byte value; draft-17 cannot read it.
		let bytes = Bytes::from_static(&[0x0b, 8, 2, 0xfc, 0, 0, 0, 0, 0, 1]);
		let decoded = read(&bytes, Version::Draft18, None).unwrap();
		assert_eq!(write(&decoded.properties, Version::Draft22).unwrap(), bytes);
		assert!(read(&bytes, Version::Draft17, None).is_err());
		assert!(matches!(
			write(&decoded.properties, Version::Draft17),
			Err(Error::Unsupported)
		));
	}
}

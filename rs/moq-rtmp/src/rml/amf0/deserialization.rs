//! Validated views into a message. Containers retain their encoded bytes instead
//! of allocating a node for every value the peer supplies.

use super::{Amf0DeserializationError as Error, markers};

#[derive(Clone, Copy, Debug)]
pub enum Value<'a> {
	Number(f64),
	Boolean(bool),
	Utf8String(&'a str),
	Object(Properties<'a>),
	StrictArray(Values<'a>),
	Null,
	Undefined,
}

#[derive(Clone, Copy, Debug)]
pub struct Values<'a> {
	bytes: &'a [u8],
	count: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Properties<'a> {
	bytes: &'a [u8],
}

impl<'a> Values<'a> {
	/// Validate the entire message, including fields the session will not read.
	pub fn decode(mut bytes: &'a [u8]) -> Result<Self, Error> {
		let input = bytes;
		let mut count = 0;
		while !bytes.is_empty() {
			read_value(&mut bytes, 0)?;
			count += 1;
		}
		Ok(Self { bytes: input, count })
	}

	pub fn len(&self) -> usize {
		self.count
	}
}

impl<'a> Iterator for Values<'a> {
	type Item = Value<'a>;

	fn next(&mut self) -> Option<Self::Item> {
		if self.count == 0 {
			return None;
		}
		self.count -= 1;
		// The immutable slice was validated before constructing this view.
		Some(read_value(&mut self.bytes, 0).expect("validated AMF0 value"))
	}
}

impl<'a> Iterator for Properties<'a> {
	type Item = (&'a str, Value<'a>);

	fn next(&mut self) -> Option<Self::Item> {
		if self.bytes.is_empty() {
			return None;
		}
		let name = read_string(&mut self.bytes).expect("validated AMF0 property name");
		let value = read_value(&mut self.bytes, 0).expect("validated AMF0 property");
		Some((name, value))
	}
}

impl<'a> Properties<'a> {
	pub fn get(self, name: &str) -> Option<Value<'a>> {
		// Match the owned map's last-value-wins behavior without retaining keys.
		self.filter(|(key, _)| *key == name).map(|(_, value)| value).last()
	}
}

impl<'a> Value<'a> {
	pub fn get_number(self) -> Option<f64> {
		if let Self::Number(value) = self {
			Some(value)
		} else {
			None
		}
	}

	pub fn get_boolean(self) -> Option<bool> {
		if let Self::Boolean(value) = self {
			Some(value)
		} else {
			None
		}
	}

	pub fn get_string(self) -> Option<&'a str> {
		if let Self::Utf8String(value) = self {
			Some(value)
		} else {
			None
		}
	}

	pub fn get_object_properties(self) -> Option<Properties<'a>> {
		if let Self::Object(value) = self {
			Some(value)
		} else {
			None
		}
	}
}

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Result<&'a [u8], Error> {
	let (value, rest) = bytes.split_at_checked(length).ok_or(Error::UnexpectedEof)?;
	*bytes = rest;
	Ok(value)
}

fn read_string<'a>(bytes: &mut &'a [u8]) -> Result<&'a str, Error> {
	let length = u16::from_be_bytes(take(bytes, 2)?.try_into().unwrap()) as usize;
	Ok(std::str::from_utf8(take(bytes, length)?)?)
}

fn read_value<'a>(bytes: &mut &'a [u8], depth: usize) -> Result<Value<'a>, Error> {
	let marker = take(bytes, 1)?[0];
	if matches!(
		marker,
		markers::OBJECT_MARKER | markers::ECMA_ARRAY_MARKER | markers::STRICT_ARRAY_MARKER
	) && depth >= 64
	{
		return Err(Error::MaxDepthExceeded);
	}
	Ok(match marker {
		markers::NUMBER_MARKER => Value::Number(f64::from_be_bytes(take(bytes, 8)?.try_into().unwrap())),
		markers::BOOLEAN_MARKER => Value::Boolean(take(bytes, 1)?[0] == 1),
		markers::STRING_MARKER => Value::Utf8String(read_string(bytes)?),
		markers::NULL_MARKER => Value::Null,
		markers::UNDEFINED_MARKER => Value::Undefined,
		markers::OBJECT_MARKER | markers::ECMA_ARRAY_MARKER => {
			if marker == markers::ECMA_ARRAY_MARKER {
				// ECMA counts are advisory; the object terminator delimits the entries.
				take(bytes, 4)?;
			}
			let start = *bytes;
			loop {
				if bytes.starts_with(&[0, 0, markers::OBJECT_END_MARKER]) {
					let properties = &start[..start.len() - bytes.len()];
					take(bytes, 3)?;
					break Value::Object(Properties { bytes: properties });
				}
				read_string(bytes)?;
				read_value(bytes, depth + 1)?;
			}
		}
		markers::STRICT_ARRAY_MARKER => {
			let count = u32::from_be_bytes(take(bytes, 4)?.try_into().unwrap()) as usize;
			let start = *bytes;
			for _ in 0..count {
				read_value(bytes, depth + 1)?;
			}
			Value::StrictArray(Values {
				bytes: &start[..start.len() - bytes.len()],
				count,
			})
		}
		_ => return Err(Error::UnknownMarker { marker }),
	})
}

#[cfg(test)]
pub fn deserialize<R: std::io::Read>(bytes: &mut R) -> Result<Vec<super::Amf0Value>, Error> {
	let mut input = Vec::new();
	bytes.read_to_end(&mut input)?;
	Ok(Values::decode(&input)?.map(Value::into_owned).collect())
}

#[cfg(test)]
impl Value<'_> {
	fn into_owned(self) -> super::Amf0Value {
		use super::Amf0Value;
		match self {
			Self::Number(value) => Amf0Value::Number(value),
			Self::Boolean(value) => Amf0Value::Boolean(value),
			Self::Utf8String(value) => Amf0Value::Utf8String(value.to_owned()),
			Self::Object(properties) => Amf0Value::Object(
				properties
					.map(|(key, value)| (key.to_owned(), value.into_owned()))
					.collect(),
			),
			Self::StrictArray(values) => Amf0Value::StrictArray(values.map(Value::into_owned).collect()),
			Self::Null => Amf0Value::Null,
			Self::Undefined => Amf0Value::Undefined,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::super::Amf0Value;
	use super::super::errors::Amf0DeserializationError;
	use super::super::markers;
	use super::deserialize;
	use byteorder::{BigEndian, WriteBytesExt};
	use std::collections::HashMap;
	use std::io::{Cursor, Read};

	#[test]
	fn can_deserialize_strict_array() {
		let mut vector = vec![];
		vector.push(markers::STRICT_ARRAY_MARKER);
		vector.write_u32::<BigEndian>(2).unwrap();
		vector.push(markers::NUMBER_MARKER);
		vector.write_f64::<BigEndian>(1.0).unwrap();
		vector.push(markers::NUMBER_MARKER);
		vector.write_f64::<BigEndian>(2.0).unwrap();

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let mut array = Vec::new();

		array.push(Amf0Value::Number(1.0));
		array.push(Amf0Value::Number(2.0));

		let expected = vec![Amf0Value::StrictArray(array)];
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_number() {
		let number: f64 = 332.0;

		let mut vector = vec![];
		vector.write_u8(markers::NUMBER_MARKER).unwrap();
		vector.write_f64::<BigEndian>(number).unwrap();

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let expected = vec![Amf0Value::Number(number)];
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_true_boolean() {
		let mut vector = vec![];
		vector.write_u8(markers::BOOLEAN_MARKER).unwrap();
		vector.write_u8(1).unwrap();

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let expected = vec![Amf0Value::Boolean(true)];
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_false_boolean() {
		let mut vector = vec![];
		vector.write_u8(markers::BOOLEAN_MARKER).unwrap();
		vector.write_u8(0).unwrap();

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let expected = vec![Amf0Value::Boolean(false)];
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_string() {
		let value = "test";

		let mut vector = vec![];
		vector.write_u8(markers::STRING_MARKER).unwrap();
		vector.write_u16::<BigEndian>(value.len() as u16).unwrap();
		vector.extend(value.as_bytes());

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let expected = vec![Amf0Value::Utf8String(value.to_string())];
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_null() {
		let mut vector = vec![];
		vector.write_u8(markers::NULL_MARKER).unwrap();

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let expected = vec![Amf0Value::Null];
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_object() {
		const NUMBER: f64 = 332.0;

		let mut vector = vec![];
		vector.push(markers::OBJECT_MARKER);
		vector.write_u16::<BigEndian>(4).unwrap();
		vector.extend("test".as_bytes());
		vector.push(markers::NUMBER_MARKER);
		vector.write_f64::<BigEndian>(NUMBER).unwrap();
		vector.write_u16::<BigEndian>(markers::UTF_8_EMPTY_MARKER).unwrap();
		vector.push(markers::OBJECT_END_MARKER);

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let mut properties = HashMap::new();
		properties.insert("test".to_string(), Amf0Value::Number(NUMBER));

		let expected = vec![Amf0Value::Object(properties)];
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_emca_array() {
		let mut vector = vec![];
		vector.push(markers::ECMA_ARRAY_MARKER);
		vector.write_u32::<BigEndian>(2).unwrap();
		vector.write_u16::<BigEndian>(5).unwrap();
		vector.extend("test1".as_bytes());
		vector.push(markers::NUMBER_MARKER);
		vector.write_f64::<BigEndian>(1.0).unwrap();
		vector.write_u16::<BigEndian>(5).unwrap();
		vector.extend("test2".as_bytes());
		vector.write_u8(markers::STRING_MARKER).unwrap();
		vector.write_u16::<BigEndian>(6).unwrap();
		vector.extend("second".as_bytes());
		vector.write_u16::<BigEndian>(markers::UTF_8_EMPTY_MARKER).unwrap();
		vector.push(markers::OBJECT_END_MARKER);

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let mut properties = HashMap::new();
		properties.insert("test1".to_string(), Amf0Value::Number(1.0));
		properties.insert("test2".to_string(), Amf0Value::Utf8String("second".to_string()));

		let expected = vec![Amf0Value::Object(properties)];
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_undefined() {
		let mut vector = vec![];
		vector.write_u8(markers::UNDEFINED_MARKER).unwrap();

		let mut input = Cursor::new(vector);
		let result = deserialize(&mut input).unwrap();

		let expected = vec![Amf0Value::Undefined];
		assert_eq!(result, expected);
	}

	fn assert_eof(result: Result<Vec<Amf0Value>, Amf0DeserializationError>) {
		match result {
			Err(Amf0DeserializationError::UnexpectedEof) => {}
			other => panic!("expected UnexpectedEof, got {other:?}"),
		}
	}

	#[test]
	fn truncated_string_errors() {
		let mut vector = vec![markers::STRING_MARKER];
		vector.write_u16::<BigEndian>(16).unwrap();
		vector.push(b'a');

		assert_eof(deserialize(&mut Cursor::new(vector)));
	}

	#[test]
	fn truncated_property_name_errors() {
		// The name declares one byte more than the rest of the input holds.
		let mut name = vec![markers::OBJECT_MARKER];
		name.write_u16::<BigEndian>(17).unwrap();
		name.extend("test".as_bytes());

		let mut rest = vec![markers::NUMBER_MARKER];
		rest.write_f64::<BigEndian>(1.0).unwrap();
		rest.write_u16::<BigEndian>(markers::UTF_8_EMPTY_MARKER).unwrap();
		rest.push(markers::OBJECT_END_MARKER);

		// A chained reader returns a short read at the boundary, so a single `read`
		// would pad the name with NULs and still find a value after it.
		assert_eof(deserialize(&mut name.as_slice().chain(rest.as_slice())));
	}
}

#[cfg(test)]
mod bounds {
	use super::*;
	use crate::rml::amf0::allocation::measure;

	#[test]
	fn views_allocate_nothing_for_dense_values_and_properties() {
		for size in [1024, 65536, 0x00ff_ffff] {
			let nulls = vec![markers::NULL_MARKER; size];
			let (values, peak) = measure(|| Values::decode(&nulls).unwrap());
			assert_eq!(values.len(), size);
			assert_eq!(peak, 0);
		}

		let mut properties = vec![markers::OBJECT_MARKER];
		for index in 0..65536u32 {
			let name = index.to_string();
			properties.extend_from_slice(&(name.len() as u16).to_be_bytes());
			properties.extend_from_slice(name.as_bytes());
			properties.push(markers::NULL_MARKER);
		}
		properties.extend_from_slice(&[0, 0, markers::OBJECT_END_MARKER]);
		let (_, peak) = measure(|| {
			let Value::Object(object) = Values::decode(&properties).unwrap().next().unwrap() else {
				panic!()
			};
			assert_eq!(object.count(), 65536);
		});
		assert_eq!(peak, 0);
	}

	#[test]
	fn skipped_containers_are_fully_validated() {
		for bytes in [
			vec![markers::STRICT_ARRAY_MARKER, 0, 0, 0, 2, markers::NULL_MARKER],
			vec![markers::STRICT_ARRAY_MARKER, 0xff, 0xff, 0xff, 0xff],
			vec![markers::OBJECT_MARKER, 0, 1, b'a', markers::NULL_MARKER],
			vec![markers::OBJECT_MARKER, 0, 1, 0xff, markers::NULL_MARKER, 0, 0, 9],
			vec![markers::STRING_MARKER, 0, 1, 0xff],
			vec![markers::OBJECT_END_MARKER],
			vec![markers::NULL_MARKER, 0xff],
		] {
			assert!(Values::decode(&bytes).is_err(), "accepted {bytes:?}");
		}
		for depth in [64, 65] {
			let mut bytes = Vec::new();
			for _ in 0..depth {
				bytes.extend_from_slice(&[markers::STRICT_ARRAY_MARKER, 0, 0, 0, 1]);
			}
			bytes.push(markers::NULL_MARKER);
			assert_eq!(Values::decode(&bytes).is_ok(), depth == 64);
		}
	}

	#[test]
	fn object_views_preserve_empty_names_and_last_values() {
		let bytes = [
			markers::OBJECT_MARKER,
			0,
			0,
			markers::NULL_MARKER,
			0,
			1,
			b'a',
			markers::NULL_MARKER,
			0,
			1,
			b'a',
			markers::BOOLEAN_MARKER,
			1,
			0,
			0,
			markers::OBJECT_END_MARKER,
		];
		let Value::Object(object) = Values::decode(&bytes).unwrap().next().unwrap() else {
			panic!()
		};
		assert!(matches!(object.get(""), Some(Value::Null)));
		assert!(matches!(object.get("a"), Some(Value::Boolean(true))));
	}
}

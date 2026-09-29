//! This module contains functionality to deserialize values from bytes
//! that were encoded via the AMF0 specification
//! (http://wwwimages.adobe.com/content/dam/Adobe/en/devnet/amf/pdf/amf0-file-format-specification.pdf)

use super::Amf0Value;
use super::errors::Amf0DeserializationError;
use super::markers;
use byteorder::{BigEndian, ReadBytesExt};
use std::collections::HashMap;
use std::io::Read;

struct ObjectProperty {
	label: String,
	value: Amf0Value,
}

/// Turns any readable byte stream and converts it into an array of AMF0 values
pub fn deserialize<R: Read>(bytes: &mut R) -> Result<Vec<Amf0Value>, Amf0DeserializationError> {
	let mut results = vec![];

	loop {
		match read_next_value(bytes, 0)? {
			Some(x) => results.push(x),
			None => break,
		};
	}

	Ok(results)
}

fn read_next_value<R: Read>(bytes: &mut R, depth: usize) -> Result<Option<Amf0Value>, Amf0DeserializationError> {
	let mut buffer: [u8; 1] = [0];
	let bytes_read = bytes.read(&mut buffer)?;

	if bytes_read == 0 {
		return Ok(None);
	}

	if buffer[0] == markers::OBJECT_END_MARKER {
		return Ok(None);
	}

	// Count containers, allowing scalar leaves inside the 64th container.
	if matches!(
		buffer[0],
		markers::OBJECT_MARKER | markers::ECMA_ARRAY_MARKER | markers::STRICT_ARRAY_MARKER
	) && depth >= 64
	{
		return Err(Amf0DeserializationError::MaxDepthExceeded);
	}

	match buffer[0] {
		markers::BOOLEAN_MARKER => parse_bool(bytes).map(Some),
		markers::NULL_MARKER => parse_null().map(Some),
		markers::UNDEFINED_MARKER => parse_undefined().map(Some),
		markers::NUMBER_MARKER => parse_number(bytes).map(Some),
		markers::OBJECT_MARKER => parse_object(bytes, depth + 1).map(Some),
		markers::ECMA_ARRAY_MARKER => parse_ecma_array(bytes, depth + 1).map(Some),
		markers::STRING_MARKER => parse_string(bytes).map(Some),
		markers::STRICT_ARRAY_MARKER => parse_strict_array(bytes, depth + 1).map(Some),
		_ => Err(Amf0DeserializationError::UnknownMarker { marker: buffer[0] }),
	}
}

fn parse_number<R: Read>(bytes: &mut R) -> Result<Amf0Value, Amf0DeserializationError> {
	let number = bytes.read_f64::<BigEndian>()?;
	let value = Amf0Value::Number(number);

	Ok(value)
}

fn parse_null() -> Result<Amf0Value, Amf0DeserializationError> {
	Ok(Amf0Value::Null)
}

fn parse_undefined() -> Result<Amf0Value, Amf0DeserializationError> {
	Ok(Amf0Value::Undefined)
}

fn parse_bool<R: Read>(bytes: &mut R) -> Result<Amf0Value, Amf0DeserializationError> {
	let value = bytes.read_u8()?;

	if value == 1 {
		Ok(Amf0Value::Boolean(true))
	} else {
		Ok(Amf0Value::Boolean(false))
	}
}

fn parse_string<R: Read>(bytes: &mut R) -> Result<Amf0Value, Amf0DeserializationError> {
	let length = bytes.read_u16::<BigEndian>()?;
	let mut buffer: Vec<u8> = vec![0_u8; length as usize];
	bytes.read(&mut buffer)?;

	let value = String::from_utf8(buffer)?;
	Ok(Amf0Value::Utf8String(value))
}

fn parse_object<R: Read>(bytes: &mut R, depth: usize) -> Result<Amf0Value, Amf0DeserializationError> {
	let mut properties = HashMap::new();

	loop {
		match parse_object_property(bytes, depth)? {
			Some(property) => properties.insert(property.label, property.value),
			None => break,
		};
	}

	let deserialized_value = Amf0Value::Object(properties);
	Ok(deserialized_value)
}

fn parse_ecma_array<R: Read>(bytes: &mut R, depth: usize) -> Result<Amf0Value, Amf0DeserializationError> {
	// An ECMA array is an array of values indexed via strings instead of numeric indexes (so
	// essentially a hash map).  It seems functionally equivalent to an object so for simplicity
	// treat it as such.

	// While the spec says it gives you the count of items in the array, it is vague about if
	// the object end marker is used.  In real world usages I have found the associative array
	// actually ends with a 0x000009 ending (same as objects do).  If we don't consume this
	// then the buffer will start at that ending and funky things will happen.  So for now it seems
	// like we can ignore the associative count and just read exactly as we would an object.

	let _associative_count = bytes.read_u32::<BigEndian>()?;
	parse_object(bytes, depth)
}

fn parse_strict_array<R: Read>(bytes: &mut R, depth: usize) -> Result<Amf0Value, Amf0DeserializationError> {
	let _array_count = bytes.read_u32::<BigEndian>()?;
	let mut values: Vec<Amf0Value> = Vec::new();

	for _ in 0.._array_count {
		match read_next_value(bytes, depth)? {
			Some(value) => {
				values.push(value);
			}
			None => break,
		};
	}

	Ok(Amf0Value::StrictArray(values))
}

fn parse_object_property<R: Read>(
	bytes: &mut R,
	depth: usize,
) -> Result<Option<ObjectProperty>, Amf0DeserializationError> {
	let label_length = bytes.read_u16::<BigEndian>()?;
	if label_length == 0 {
		// Next byte should be the end of object marker.  We need to read this
		// to make sure we progress the current position.
		let byte = bytes.read_u8()?;
		if byte != markers::OBJECT_END_MARKER {
			return Err(Amf0DeserializationError::UnexpectedEmptyObjectPropertyName);
		}

		return Ok(None);
	}

	let mut label_buffer = vec![0; label_length as usize];
	bytes.read(&mut label_buffer)?;

	let label = String::from_utf8(label_buffer)?;

	match read_next_value(bytes, depth)? {
		None => Err(Amf0DeserializationError::UnexpectedEof),
		Some(property_value) => Ok(Some(ObjectProperty {
			label,
			value: property_value,
		})),
	}
}

#[cfg(test)]
mod tests {
	use super::super::Amf0Value;
	use super::super::markers;
	use super::deserialize;
	use byteorder::{BigEndian, WriteBytesExt};
	use std::collections::HashMap;
	use std::io::Cursor;

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
}

use bytes::Bytes;
use rml_amf0;
use rml_amf0::Amf0Value;
use std::io::Cursor;

use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};

pub fn serialize(
	command_name: String,
	transaction_id: f64,
	command_object: Amf0Value,
	mut additional_arguments: Vec<Amf0Value>,
) -> Result<Bytes, MessageSerializationError> {
	let mut values = vec![
		Amf0Value::Utf8String(command_name),
		Amf0Value::Number(transaction_id),
		command_object,
	];

	values.append(&mut additional_arguments);
	let bytes = rml_amf0::serialize(&values)?;

	Ok(Bytes::from(bytes))
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	let mut cursor = Cursor::new(data);
	let mut arguments = rml_amf0::deserialize(&mut cursor)?;

	let command_name: String;
	let transaction_id: f64;
	let command_object: Amf0Value;
	// This deserializes untrusted input; a command with fewer than the 3 required
	// values would panic the drain(..3) below, so reject it first.
	if arguments.len() < 3 {
		return Err(MessageDeserializationError::InvalidMessageFormat);
	}
	{
		let mut arg_iterator = arguments.drain(..3);

		command_name = match arg_iterator
			.next()
			.ok_or(MessageDeserializationError::InvalidMessageFormat)?
		{
			Amf0Value::Utf8String(value) => value,
			_ => return Err(MessageDeserializationError::InvalidMessageFormat),
		};

		transaction_id = match arg_iterator
			.next()
			.ok_or(MessageDeserializationError::InvalidMessageFormat)?
		{
			Amf0Value::Number(value) => value,
			_ => return Err(MessageDeserializationError::InvalidMessageFormat),
		};

		command_object = arg_iterator
			.next()
			.ok_or(MessageDeserializationError::InvalidMessageFormat)?;
	}

	Ok(RtmpMessage::Amf0Command {
		command_name,
		transaction_id,
		command_object,
		additional_arguments: arguments,
	})
}

#[cfg(test)]
mod tests {
	use super::{deserialize, serialize};
	use bytes::Bytes;
	use rml_amf0;
	use rml_amf0::Amf0Value;
	use std::collections::HashMap;
	use std::io::Cursor;

	use crate::rml::messages::RtmpMessage;

	#[test]
	fn short_command_is_rejected_not_panicked() {
		// A command with fewer than the 3 required values must error, not panic
		// the drain(..3) (this path deserializes untrusted network input).
		for values in [vec![], vec![Amf0Value::Utf8String("connect".to_string())]] {
			let bytes = Bytes::from(rml_amf0::serialize(&values).unwrap());
			assert!(deserialize(bytes).is_err());
		}
	}

	#[test]
	fn can_serialize_message() {
		let mut properties1 = HashMap::new();
		properties1.insert("prop1".to_string(), Amf0Value::Utf8String("abc".to_string()));
		properties1.insert("prop2".to_string(), Amf0Value::Null);

		let mut properties2 = HashMap::new();
		properties2.insert("prop1".to_string(), Amf0Value::Utf8String("abc".to_string()));
		properties2.insert("prop2".to_string(), Amf0Value::Null);

		let raw_message = serialize(
			"test".to_string(),
			23.0,
			Amf0Value::Object(properties1),
			vec![Amf0Value::Boolean(true), Amf0Value::Number(52.0)],
		)
		.unwrap();

		let mut cursor = Cursor::new(raw_message);
		let result = rml_amf0::deserialize(&mut cursor).unwrap();

		let expected = vec![
			Amf0Value::Utf8String("test".to_string()),
			Amf0Value::Number(23.0),
			Amf0Value::Object(properties2),
			Amf0Value::Boolean(true),
			Amf0Value::Number(52.0),
		];

		assert_eq!(expected, result);
	}

	#[test]
	fn can_deserialize_message() {
		let mut properties1 = HashMap::new();
		properties1.insert("prop1".to_string(), Amf0Value::Utf8String("abc".to_string()));
		properties1.insert("prop2".to_string(), Amf0Value::Null);

		let mut properties2 = HashMap::new();
		properties2.insert("prop1".to_string(), Amf0Value::Utf8String("abc".to_string()));
		properties2.insert("prop2".to_string(), Amf0Value::Null);

		let values = vec![
			Amf0Value::Utf8String("test".to_string()),
			Amf0Value::Number(23.0),
			Amf0Value::Object(properties1),
			Amf0Value::Boolean(true),
			Amf0Value::Number(52.0),
		];

		let bytes = Bytes::from(rml_amf0::serialize(&values).unwrap());
		let expected = RtmpMessage::Amf0Command {
			command_name: "test".to_string(),
			transaction_id: 23.0,
			command_object: Amf0Value::Object(properties2),
			additional_arguments: vec![Amf0Value::Boolean(true), Amf0Value::Number(52.0)],
		};
		let result = deserialize(bytes).unwrap();

		assert_eq!(expected, result);
	}
}

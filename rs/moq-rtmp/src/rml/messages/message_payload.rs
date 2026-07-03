use super::types;
use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};
use crate::rml::time::RtmpTimestamp;
use bytes::Bytes;
use std::fmt;

/// Represents a raw RTMP message
#[derive(PartialEq)]
pub struct MessagePayload {
	pub timestamp: RtmpTimestamp,
	pub type_id: u8,
	pub message_stream_id: u32,
	pub data: Bytes,
}

impl fmt::Debug for MessagePayload {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(
			f,
			"MessagePayload {{ timestamp: {:?}, type_id: {:?}, message_stream_id: {:?}, data: [..] }}",
			self.timestamp, self.type_id, self.message_stream_id
		)
	}
}

impl MessagePayload {
	/// Creates a new message payload with default values.
	///
	/// This is mostly used when all information about a message is not known at creation time
	/// but instead is built up over time (e.g. RTMP chunk deserialization process).
	pub fn new() -> MessagePayload {
		MessagePayload {
			timestamp: RtmpTimestamp::new(0),
			message_stream_id: 0,
			type_id: 0,
			data: Bytes::new(),
		}
	}

	/// Deserializes the message data in the specified payload into its corresponding
	/// `RtmpMessage`.
	///
	/// Note that flash clients (like Wowza's test client) lie and mark amf0 data and commands as
	/// amf3 values.  It is unknown why this happens, but any Amf3 command/data messages that are
	/// seen are deserialized as amf0.  So far this has not caused any issues.
	pub fn to_rtmp_message(&self) -> Result<RtmpMessage, MessageDeserializationError> {
		match self.type_id {
			1 => types::set_chunk_size::deserialize(self.data.clone()),
			2 => types::abort::deserialize(self.data.clone()),
			3 => types::acknowledgement::deserialize(self.data.clone()),
			4 => types::user_control::deserialize(self.data.clone()),
			5 => types::window_acknowledgement_size::deserialize(self.data.clone()),
			6 => types::set_peer_bandwidth::deserialize(self.data.clone()),
			8 => types::audio_data::deserialize(self.data.clone()),
			9 => types::video_data::deserialize(self.data.clone()),
			18 => types::amf0_data::deserialize(self.data.clone()),
			20 => types::amf0_command::deserialize(self.data.clone()),

			// For some reason Flash players (like wowza's test player) send messages
			// that are flagged as amf3 encoded, but in reality they are amf0 encoded
			15 => types::amf0_data::deserialize(self.data.clone()),

			17 => {
				// Fake amf3 commands usually seem to have a 0 in front of the amf0 data.
				if self.data.len() > 0 && self.data[0] == 0x00 {
					let slice = self.data.slice(1..);
					types::amf0_command::deserialize(slice)
				} else {
					types::amf0_command::deserialize(self.data.clone())
				}
			}

			_ => Ok(RtmpMessage::Unknown {
				type_id: self.type_id,
				data: self.data.clone(),
			}),
		}
	}

	/// This creates a `MessagePayload` from an `RtmpMessage`.
	///
	/// Since RTMP messages do not contain timestamp or the conversation stream id these must be
	/// provided at the time of creation.
	pub fn from_rtmp_message(
		message: RtmpMessage,
		timestamp: RtmpTimestamp,
		message_stream_id: u32,
	) -> Result<MessagePayload, MessageSerializationError> {
		let type_id = message.get_message_type_id();

		let bytes = match message {
			RtmpMessage::Unknown { type_id: _, data } => data,

			RtmpMessage::Abort { stream_id } => types::abort::serialize(stream_id)?,

			RtmpMessage::Acknowledgement { sequence_number } => types::acknowledgement::serialize(sequence_number)?,

			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			} => types::amf0_command::serialize(command_name, transaction_id, command_object, additional_arguments)?,

			RtmpMessage::Amf0Data { values } => types::amf0_data::serialize(values)?,

			RtmpMessage::AudioData { data } => types::audio_data::serialize(data)?,

			RtmpMessage::SetChunkSize { size } => types::set_chunk_size::serialize(size)?,

			RtmpMessage::SetPeerBandwidth { size, limit_type } => {
				types::set_peer_bandwidth::serialize(limit_type, size)?
			}

			RtmpMessage::UserControl {
				event_type,
				stream_id,
				buffer_length,
				timestamp,
			} => types::user_control::serialize(event_type, stream_id, buffer_length, timestamp)?,

			RtmpMessage::VideoData { data } => types::video_data::serialize(data)?,

			RtmpMessage::WindowAcknowledgement { size } => types::window_acknowledgement_size::serialize(size)?,
		};

		Ok(MessagePayload {
			data: bytes,
			type_id,
			message_stream_id,
			timestamp,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::{MessagePayload, RtmpMessage};
	use crate::rml::messages::{PeerBandwidthLimitType, UserControlEventType};
	use crate::rml::time::RtmpTimestamp;
	use bytes::{BufMut, Bytes, BytesMut};
	use rml_amf0::Amf0Value;

	#[test]
	fn can_get_payload_from_abort_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::Abort { stream_id: 23 };
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 2, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_acknowledgement_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::Acknowledgement { sequence_number: 23 };
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_amf0_command_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::Amf0Command {
			command_name: "test".to_string(),
			command_object: Amf0Value::Null,
			transaction_id: 23.0,
			additional_arguments: vec![],
		};

		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 20, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_amf0_data_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::Amf0Data {
			values: vec![Amf0Value::Number(23.0)],
		};
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 18, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_audio_data_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::AudioData {
			data: Bytes::from(vec![33_u8]),
		};
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 8, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_set_chunk_size_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::SetChunkSize { size: 33 };
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 1, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_set_peer_bandwidth_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::SetPeerBandwidth {
			size: 33,
			limit_type: PeerBandwidthLimitType::Hard,
		};
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 6, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_user_control_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::UserControl {
			event_type: UserControlEventType::StreamBegin,
			stream_id: Some(33),
			timestamp: None,
			buffer_length: None,
		};

		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 4, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_video_data_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::VideoData {
			data: Bytes::from(vec![23_u8]),
		};
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 9, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_window_acknowledgement_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::WindowAcknowledgement { size: 23 };
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 5, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_payload_from_unknown_message() {
		let timestamp = RtmpTimestamp::new(55);
		let stream_id = 52;
		let message = RtmpMessage::Unknown {
			type_id: 33,
			data: Bytes::from(vec![23_u8]),
		};
		let result = MessagePayload::from_rtmp_message(message, timestamp, stream_id).unwrap();

		assert_ne!(result.data.len(), 0, "Empty payload data seen");
		assert_eq!(result.type_id, 33, "Incorrect type id");
		assert_eq!(result.message_stream_id, stream_id, "Incorrect message stream id");
		assert_eq!(result.timestamp, 55, "Incorrect timestamp");
	}

	#[test]
	fn can_get_rtmp_message_for_abort_payload() {
		let message = RtmpMessage::Abort { stream_id: 15 };
		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_acknowledgement_payload() {
		let message = RtmpMessage::Acknowledgement { sequence_number: 15 };
		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_amf0_command_payload() {
		let message = RtmpMessage::Amf0Command {
			command_name: "test".to_string(),
			transaction_id: 15.0,
			command_object: Amf0Value::Number(23.0),
			additional_arguments: vec![Amf0Value::Null],
		};

		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_amf0_data_payload() {
		let message = RtmpMessage::Amf0Data {
			values: vec![Amf0Value::Number(23.3)],
		};
		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_audio_data_payload() {
		let message = RtmpMessage::AudioData {
			data: Bytes::from(vec![3_u8]),
		};
		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_set_chunk_size_payload() {
		let message = RtmpMessage::SetChunkSize { size: 15 };
		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_set_peer_bandwidth_payload() {
		let message = RtmpMessage::SetPeerBandwidth {
			size: 15,
			limit_type: PeerBandwidthLimitType::Hard,
		};
		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_user_control_payload() {
		let message = RtmpMessage::UserControl {
			stream_id: Some(15),
			buffer_length: None,
			timestamp: None,
			event_type: UserControlEventType::StreamBegin,
		};

		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_video_data_payload() {
		let message = RtmpMessage::VideoData {
			data: Bytes::from(vec![3_u8]),
		};
		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_window_acknowledgement_payload() {
		let message = RtmpMessage::WindowAcknowledgement { size: 25 };
		let payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_amf0_command_flagged_as_amf3() {
		let message = RtmpMessage::Amf0Command {
			command_name: "test".to_string(),
			transaction_id: 15.0,
			command_object: Amf0Value::Number(23.0),
			additional_arguments: vec![Amf0Value::Null],
		};

		let mut payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		payload.type_id = 17;

		let mut new_data = BytesMut::with_capacity(payload.data.len() + 1);
		new_data.put_u8(0);
		new_data.extend_from_slice(&payload.data);
		payload.data = new_data.freeze();

		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}

	#[test]
	fn can_get_rtmp_message_for_amf0_data_payload_flagged_as_amf3() {
		let message = RtmpMessage::Amf0Data {
			values: vec![Amf0Value::Number(23.3)],
		};
		let mut payload = MessagePayload::from_rtmp_message(message.clone(), RtmpTimestamp::new(0), 15).unwrap();
		payload.type_id = 15;

		let result = payload.to_rtmp_message().unwrap();

		assert_eq!(result, message);
	}
}

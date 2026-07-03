/*!
This module contains all the RTMP message types as well as functionality for serializing
and deserializing these messages into payloads.

`MessagePayload`s have auxiliary data about an RTMP message, such as what message stream it is
meant for, the timestamp for the message and what type of message it is.
*/

mod deserialization_errors;
mod message_payload;
mod serialization_errors;
mod types;

pub use self::deserialization_errors::MessageDeserializationError;
pub use self::message_payload::MessagePayload;
pub use self::serialization_errors::MessageSerializationError;
use crate::rml::time::RtmpTimestamp;
use bytes::Bytes;
use rml_amf0::Amf0Value;

/// The type of bandwidth limiting that is being requested
#[derive(Eq, PartialEq, Debug, Clone)]
pub enum PeerBandwidthLimitType {
	/// Peer should limit its output bandwidth to the indicated window size
	Hard,

	/// The peer should limit it's output bandwidth to the window indicated or the limit
	/// already in effect, whichever is smaller.
	Soft,

	/// If we previously had a hard limit, this limit should be treated as hard.  Otherwise ignore.
	Dynamic,
}

/// Events and notifications that are raised with the peer
#[derive(Eq, PartialEq, Debug, Clone)]
pub enum UserControlEventType {
	/// Notifies the client that a stream has become functional
	StreamBegin,

	/// Notifies the client that the playback of data on the stream is over
	StreamEof,

	/// Notifies the client that there is no more data on the stream.
	StreamDry,

	/// Notifies the server of the buffer size (in milliseconds) that the client is using
	SetBufferLength,

	/// Notifies the client that the stream is a recorded stream.
	StreamIsRecorded,

	/// Server sends this to test whether the client is reachable.
	PingRequest,

	/// Client sends this in response to a ping request
	PingResponse,

	/// Buffer Empty (unofficial name): After the server has sent a complete buffer, and
	/// sends this Buffer Empty message, it will wait until the play
	/// duration of that buffer has passed before sending a new buffer.
	/// The Buffer Ready message will be sent when the new buffer starts.
	BufferEmpty,

	/// Buffer Ready (unofficial name): After the server has sent a complete buffer, and
	/// sends a Buffer Empty message, it will wait until the play
	/// duration of that buffer has passed before sending a new buffer.
	/// The Buffer Ready message will be sent when the new buffer starts.
	/// (There is no BufferReady message for the very first buffer;
	/// presumably the Stream Begin message is sufficient for that
	/// purpose.)
	BufferReady,
}

/// An enumeration of all types of RTMP messages that are supported
#[derive(PartialEq, Debug, Clone)]
pub enum RtmpMessage {
	/// This type of message is used when an RTMP message is encountered with a type id that
	/// we do not know about
	Unknown { type_id: u8, data: Bytes },

	/// Notifies the peer that if it is waiting for chunks to complete a message that it should
	/// discard the chunks it has already received.
	Abort { stream_id: u32 },

	/// An acknowledgement sent to confirm how many bytes that has been received since the prevoius
	/// acknowledgement.
	Acknowledgement { sequence_number: u32 },

	/// A command being sent, encoded with amf0 values
	Amf0Command {
		command_name: String,
		transaction_id: f64,
		command_object: Amf0Value,
		additional_arguments: Vec<Amf0Value>,
	},

	/// A message containing an array of data encoded as amf0 values
	Amf0Data { values: Vec<Amf0Value> },

	/// A message containing audio data
	AudioData { data: Bytes },

	/// Tells the peer that the maximum chunk size for RTMP chunks it will be sending is changing
	/// to the specified size.
	SetChunkSize { size: u32 },

	/// Indicates that the peer should limit its output bandwidth
	SetPeerBandwidth {
		size: u32,
		limit_type: PeerBandwidthLimitType,
	},

	/// Notifies the peer of an event, such as a stream being
	/// created or telling the peer how much of a buffer it should have.
	UserControl {
		event_type: UserControlEventType,
		stream_id: Option<u32>,
		buffer_length: Option<u32>,
		timestamp: Option<RtmpTimestamp>,
	},

	/// A message containing video data
	VideoData { data: Bytes },

	/// Notifies the peer how many bytes should be received before sending an `Acknowledgement`
	/// message
	WindowAcknowledgement { size: u32 },
}

impl RtmpMessage {
	pub fn into_message_payload(
		self,
		timestamp: RtmpTimestamp,
		message_stream_id: u32,
	) -> Result<MessagePayload, MessageSerializationError> {
		MessagePayload::from_rtmp_message(self, timestamp, message_stream_id)
	}

	pub fn get_message_type_id(&self) -> u8 {
		match *self {
			RtmpMessage::Unknown { type_id, data: _ } => type_id,
			RtmpMessage::Abort { stream_id: _ } => 2_u8,
			RtmpMessage::Acknowledgement { sequence_number: _ } => 3_u8,
			RtmpMessage::Amf0Command {
				command_name: _,
				transaction_id: _,
				command_object: _,
				additional_arguments: _,
			} => 20_u8,
			RtmpMessage::Amf0Data { values: _ } => 18_u8,
			RtmpMessage::AudioData { data: _ } => 8_u8,
			RtmpMessage::SetChunkSize { size: _ } => 1_u8,
			RtmpMessage::SetPeerBandwidth { size: _, limit_type: _ } => 6_u8,
			RtmpMessage::UserControl {
				event_type: _,
				stream_id: _,
				buffer_length: _,
				timestamp: _,
			} => 4_u8,
			RtmpMessage::VideoData { data: _ } => 9_u8,
			RtmpMessage::WindowAcknowledgement { size: _ } => 5_u8,
		}
	}
}

use crate::rml::sessions::StreamMetadata;
use crate::rml::time::RtmpTimestamp;
use bytes::Bytes;
use rml_amf0::Amf0Value;

/// Events that can be raised by the client session so that custom business logic can be written
/// to react to it
#[derive(PartialEq, Debug)]
pub enum ClientSessionEvent {
	/// Raised when a connection request has been accepted by the server
	ConnectionRequestAccepted,

	/// The server has rejected the connection request
	ConnectionRequestRejected { description: String },

	/// The server has accepted our request to play video back from a stream key
	PlaybackRequestAccepted,

	/// The server has accepted our request to publish video
	PublishRequestAccepted,

	/// The server has sent over new metadata for the stream
	StreamMetadataReceived { metadata: StreamMetadata },

	/// The server has sent over video data for the stream
	VideoDataReceived { timestamp: RtmpTimestamp, data: Bytes },

	/// The server has sent over audio data for the stream
	AudioDataReceived { timestamp: RtmpTimestamp, data: Bytes },

	/// The server sent an Amf0 command that was not able to be handled
	UnhandleableAmf0Command {
		command_name: String,
		transaction_id: f64,
		command_object: Amf0Value,
		additional_values: Vec<Amf0Value>,
	},

	/// The server sent us a result to a transaction that we don't know about
	UnknownTransactionResultReceived {
		transaction_id: f64,
		command_object: Amf0Value,
		additional_values: Vec<Amf0Value>,
	},

	/// The server sent an `onStatus` message with a `code` property that we don't know
	/// how to handle.
	UnhandleableOnStatusCode { code: String },

	/// The client has sent an acknowledgement that they have received the specified number of bytes
	AcknowledgementReceived { bytes_received: u32 },

	/// The client has responded to a ping request
	PingResponseReceived { timestamp: RtmpTimestamp },
}

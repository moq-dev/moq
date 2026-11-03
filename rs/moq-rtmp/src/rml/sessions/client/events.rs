use crate::rml::sessions::StreamMetadata;
use crate::rml::time::RtmpTimestamp;
use bytes::Bytes;

/// Events that can be raised by the client session so that custom business logic can be written
/// to react to it
#[derive(PartialEq, Debug)]
pub enum ClientSessionEvent {
	/// Raised when a connection request has been accepted by the server
	ConnectionRequestAccepted,

	/// The server has rejected the connection request
	ConnectionRequestRejected { code: String, description: String },

	/// The server has accepted our request to play video back from a stream key
	PlaybackRequestAccepted,

	/// The server has accepted our request to publish video
	PublishRequestAccepted,

	/// The server has rejected our request to play video back from a stream key
	PlaybackRequestRejected { code: String, description: String },

	/// The server has rejected our request to publish video
	PublishRequestRejected { code: String, description: String },

	/// The server has sent over new metadata for the stream
	StreamMetadataReceived { metadata: StreamMetadata },

	/// The server has sent over video data for the stream
	VideoDataReceived { timestamp: RtmpTimestamp, data: Bytes },

	/// The server has sent over audio data for the stream
	AudioDataReceived { timestamp: RtmpTimestamp, data: Bytes },

	/// The server sent an Amf0 command that was not able to be handled
	UnhandleableAmf0Command { command_name: String, transaction_id: f64 },

	/// The server sent an Amf0 data message that isn't stream metadata, such as `onCuePoint`
	UnhandleableAmf0Data {},

	/// The server sent us a result to a transaction that we don't know about
	UnknownTransactionResultReceived { transaction_id: f64 },

	/// The server sent an `onStatus` message with a `code` property that we don't know
	/// how to handle.
	UnhandleableOnStatusCode { code: String },

	/// The client has sent an acknowledgement that they have received the specified number of bytes
	AcknowledgementReceived { bytes_received: u32 },

	/// The client has responded to a ping request
	PingResponseReceived { timestamp: RtmpTimestamp },
}

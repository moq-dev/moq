use super::PublishMode;
use crate::rml::sessions::StreamMetadata;
use crate::rml::time::RtmpTimestamp;
use bytes::Bytes;
use rml_amf0::Amf0Value;

/// Represents where RTMP playback should start from
#[derive(PartialEq, Debug, Clone)]
pub enum PlayStartValue {
	/// If a live stream exists for the specified stream keyplay it, if not
	/// play the recorded stream with a matching name
	LiveOrRecorded,

	/// Only play live streams with the provided stream key
	LiveOnly,

	/// Play the recorded stream for the stream key at the specified start time
	StartTimeInSeconds(u32),
}

/// An event that a server session can raise
#[derive(Debug, PartialEq, Clone)]
pub enum ServerSessionEvent {
	/// The client is changing the maximum size of the RTMP chunks they will be sending
	ClientChunkSizeChanged { new_chunk_size: u32 },

	/// The client is requesting a connection on the specified RTMP application name.
	/// `caps_ex` is the enhanced-RTMP `capsEx` capability bitmask the client
	/// advertised in its `connect` command object (0 if absent).
	ConnectionRequested {
		request_id: u32,
		app_name: String,
		caps_ex: u32,
	},

	/// The client is requesting a stream key be released for use.
	ReleaseStreamRequested {
		request_id: u32,
		app_name: String,
		stream_key: String,
	},

	/// The client is requesting the ability to publish on the specified stream key,
	PublishStreamRequested {
		request_id: u32,
		app_name: String,
		stream_key: String,
		mode: PublishMode,
	},

	/// The client is finished publishing on the specified stream key
	PublishStreamFinished { app_name: String, stream_key: String },

	/// The client is changing metadata properties of the stream being published
	StreamMetadataChanged {
		app_name: String,
		stream_key: String,
		metadata: StreamMetadata,
	},

	/// Audio data was received from the client
	AudioDataReceived {
		app_name: String,
		stream_key: String,
		data: Bytes,
		timestamp: RtmpTimestamp,
	},

	/// Video data received from the client
	VideoDataReceived {
		app_name: String,
		stream_key: String,
		data: Bytes,
		timestamp: RtmpTimestamp,
	},

	/// The client sent an Amf0 command that was not able to be handled
	UnhandleableAmf0Command {
		command_name: String,
		transaction_id: f64,
		command_object: Amf0Value,
		additional_values: Vec<Amf0Value>,
	},

	/// The client is requesting playback of the specified stream
	PlayStreamRequested {
		request_id: u32,
		app_name: String,
		stream_key: String,
		start_at: PlayStartValue,
		duration: Option<u32>,
		reset: bool,
		stream_id: u32,
	},

	/// The client is finished with playback of the specified stream
	PlayStreamFinished { app_name: String, stream_key: String },

	/// The client has sent an acknowledgement that they have received the specified number of bytes
	AcknowledgementReceived { bytes_received: u32 },

	/// The client has responded to a ping request
	PingResponseReceived { timestamp: RtmpTimestamp },
}

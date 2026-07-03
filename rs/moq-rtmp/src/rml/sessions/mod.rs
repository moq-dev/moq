/*!
This module contains implemented session abstractions.

A session is a high level abstraction that makes it simple to create custom RTMP clients and
servers without having to worry about the exact flow of RTMP messages to perform specific
actions.  The session has it's own `ChunkSerializer` and `ChunkDeserializer` so consumers only
have to worry about bytes in and bytes/events out.

A single session represents a single peer in an RTMP connection, so if multiple connections are
being managed (in any direction) each connection should have its own, distinct, session instance.

It is also expected that a session has been created *after* handshaking has been completed.
*/

mod client;
mod server;

pub use self::client::ClientSession;
pub use self::client::ClientSessionConfig;
pub use self::client::ClientSessionError;
pub use self::client::ClientSessionEvent;
pub use self::client::ClientSessionResult;
pub use self::client::ClientState;
pub use self::client::PublishRequestType;

pub use self::server::PublishMode;
pub use self::server::ServerSession;
pub use self::server::ServerSessionConfig;
pub use self::server::ServerSessionError;
pub use self::server::ServerSessionEvent;
pub use self::server::ServerSessionResult;

use rml_amf0::Amf0Value;
use std::collections::HashMap;

/// Contains the metadata information a stream may advertise on publishing
#[derive(PartialEq, Debug, Clone)]
pub struct StreamMetadata {
	pub video_width: Option<u32>,
	pub video_height: Option<u32>,
	pub video_codec_id: Option<u32>,
	pub video_frame_rate: Option<f32>,
	pub video_bitrate_kbps: Option<u32>,
	pub audio_codec_id: Option<u32>,
	pub audio_bitrate_kbps: Option<u32>,
	pub audio_sample_rate: Option<u32>,
	pub audio_channels: Option<u32>,
	pub audio_is_stereo: Option<bool>,
	pub encoder: Option<String>,
}

impl StreamMetadata {
	/// Creates a new (and empty) metadata instance
	pub fn new() -> StreamMetadata {
		StreamMetadata {
			video_width: None,
			video_height: None,
			video_codec_id: None,
			video_frame_rate: None,
			video_bitrate_kbps: None,
			audio_codec_id: None,
			audio_bitrate_kbps: None,
			audio_sample_rate: None,
			audio_channels: None,
			audio_is_stereo: None,
			encoder: None,
		}
	}

	/// Iterates through the passed in hashmap and uses their values to set the metadata
	/// properties. The keys are based on standard metadata property names seen from existing
	/// RTMP encoders.
	pub fn apply_metadata_values(&mut self, mut properties: HashMap<String, Amf0Value>) {
		for (key, value) in properties.drain() {
			match key.as_ref() {
				"width" => match value.get_number() {
					Some(x) => self.video_width = Some(x as u32),
					None => (),
				},

				"height" => match value.get_number() {
					Some(x) => self.video_height = Some(x as u32),
					None => (),
				},

				"videocodecid" => match value.get_number() {
					Some(x) => self.video_codec_id = Some(x as u32),
					None => (),
				},

				"videodatarate" => match value.get_number() {
					Some(x) => self.video_bitrate_kbps = Some(x as u32),
					None => (),
				},

				"framerate" => match value.get_number() {
					Some(x) => self.video_frame_rate = Some(x as f32),
					None => (),
				},

				"audiocodecid" => match value.get_number() {
					Some(x) => self.audio_codec_id = Some(x as u32),
					None => (),
				},

				"audiodatarate" => match value.get_number() {
					Some(x) => self.audio_bitrate_kbps = Some(x as u32),
					None => (),
				},

				"audiosamplerate" => match value.get_number() {
					Some(x) => self.audio_sample_rate = Some(x as u32),
					None => (),
				},

				"audiochannels" => match value.get_number() {
					Some(x) => self.audio_channels = Some(x as u32),
					None => (),
				},

				"stereo" => match value.get_boolean() {
					Some(x) => self.audio_is_stereo = Some(x),
					None => (),
				},

				"encoder" => match value.get_string() {
					Some(x) => self.encoder = Some(x),
					None => (),
				},

				_ => (),
			}
		}
	}
}

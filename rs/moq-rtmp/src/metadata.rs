/// Counts the script data one RTMP source discards, warning once and summarizing on drop.
#[derive(Default)]
pub(crate) struct Dropped {
	pub(crate) count: u64,
	/// The broadcast path, logged in place of the credential-bearing stream key.
	path: String,
}

impl Dropped {
	pub(crate) fn new(path: &str) -> Self {
		Self {
			count: 0,
			path: path.to_string(),
		}
	}

	pub(crate) fn record(&mut self) {
		self.count += 1;
		if self.count == 1 {
			tracing::warn!(path = %self.path, count = self.count, "discarding RTMP stream metadata");
		}
	}
}

impl Drop for Dropped {
	fn drop(&mut self) {
		if self.count > 1 {
			tracing::warn!(path = %self.path, count = self.count, "RTMP metadata discard summary");
		}
	}
}

/// An `onCuePoint` AMF0 data message on stream 1, as an encoder serializes it.
#[cfg(test)]
pub(crate) fn cue_point_packet() -> Vec<u8> {
	use crate::rml::amf0::Amf0Value;

	let message = crate::rml::messages::RtmpMessage::Amf0Data {
		values: vec![
			Amf0Value::Utf8String("onCuePoint".into()),
			Amf0Value::Object([("name".to_string(), Amf0Value::Utf8String("ad".into()))].into()),
		],
	};
	let payload = message
		.into_message_payload(crate::rml::time::RtmpTimestamp::new(0), 1)
		.unwrap();
	let mut serializer = crate::rml::chunk_io::ChunkSerializer::new();
	serializer.serialize(&payload, false, false).unwrap().bytes
}

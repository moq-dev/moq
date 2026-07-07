/// Configuration options that govern how a RTMP client session should operate
#[derive(Clone)]
pub struct ClientSessionConfig {
	pub flash_version: String,
	pub playback_buffer_length_ms: u32,
	pub window_ack_size: u32,
	pub chunk_size: u32,
	pub tc_url: Option<String>,
}

impl ClientSessionConfig {
	/// Creates a new configuration object with default values
	pub fn new() -> ClientSessionConfig {
		ClientSessionConfig {
			flash_version: "WIN 23,0,0,207".to_string(),
			playback_buffer_length_ms: 2_000,
			window_ack_size: 2_500_000,
			chunk_size: 4096,
			tc_url: None,
		}
	}
}

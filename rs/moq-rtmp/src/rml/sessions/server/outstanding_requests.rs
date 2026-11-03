use super::PublishMode;

pub enum OutstandingRequest {
	ConnectionRequest {
		app_name: String,
		transaction_id: f64,
	},

	PublishRequested {
		stream_key: String,
		mode: PublishMode,
		stream_id: u32,
	},

	PlayRequested {
		stream_key: String,
		stream_id: u32,
	},
}

impl OutstandingRequest {
	pub fn id(&self) -> u32 {
		match self {
			Self::ConnectionRequest { .. } => 0,
			Self::PublishRequested { .. } | Self::PlayRequested { .. } => 1,
		}
	}
}

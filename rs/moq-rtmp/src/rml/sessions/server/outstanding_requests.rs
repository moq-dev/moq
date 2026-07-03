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

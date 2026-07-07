use super::PublishRequestType;

pub enum TransactionPurpose {
	PlayRequest {
		stream_key: String,
	},

	PublishRequest {
		stream_key: String,
		request_type: PublishRequestType,
	},
}

pub enum OutstandingTransaction {
	ConnectionRequested { app_name: String },

	CreateStream { purpose: TransactionPurpose },
}

use serde::{Deserialize, Serialize};

/// Chat track metadata
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
	pub message: Option<moq_net::Track>,
	pub typing: Option<moq_net::Track>,
}

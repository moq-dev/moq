//! The refusal an auth server or embedded decider answers.

use serde::{Deserialize, Serialize};

/// A bounded reason an admission decider refuses a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Reason {
	/// The credential does not allow the requested access.
	Forbidden,
	/// The verified credential has expired.
	Expired,
	/// The decider refused the credential or session.
	#[default]
	#[serde(other)]
	Refused,
}

impl std::fmt::Display for Reason {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self {
			Self::Refused => "refused",
			Self::Forbidden => "forbidden",
			Self::Expired => "expired",
		})
	}
}

/// A refusal's reason and optional attribution, returned as JSON with HTTP 403.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("the admission decider refused the session: {reason}")]
#[non_exhaustive]
pub struct Refusal {
	/// Why admission was refused; unknown JSON reasons read as `refused`.
	pub reason: Reason,
	/// A root named by a verified credential, never an unverified request path.
	pub root: Option<String>,
	/// The tier resolved by the decider; absent uses the default tier.
	pub tier: Option<String>,
}

use moq_pattern::Patterns;
use serde::{Deserialize, Serialize};

/// Who a token was issued to: its subject, qualified by the issuer that named it, so two
/// issuers' subjects never collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Subject {
	/// The token's issuer (`iss`), or `None` when it names none.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub issuer: Option<String>,

	/// The token's subject (`sub`), unique within its issuer.
	pub name: String,
}

/// Whether a connection that dialed `path` with this publish grant may publish only part of
/// the tree, so the routes it forwards are filed under its subject. Anything but `**` at the
/// root is scoped; a grant that publishes nothing forwards no routes.
pub(crate) fn scoped(path: &str, publish: &Patterns) -> bool {
	!publish.is_empty() && !(crate::path::normalize(path).is_empty() && publish.contains(&moq_pattern::Pattern::all()))
}

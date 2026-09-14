//! The LiveKit AccessToken analogue: minting a moq-auth token rooted at the room prefix.
//!
//! This crate has no service and no storage. Joining a room is signing a token
//! with these claims and dialing the relay at that root.

/// Token claims for a participant in `room`.
///
/// `root` is the room prefix, `subscribe` is `**` (everything under the room),
/// and `publish` is `<identity>/**` so a participant cannot publish at anyone
/// else's paths.
pub fn claims(room: impl Into<String>, identity: &str) -> Result<moq_auth::Claims, crate::Error> {
	let identity = moq_net::Path::new(identity);
	if identity.is_empty() {
		return Err(crate::Error::EmptyIdentity);
	}
	// A normalized non-empty path holds no `*`, so it is always a valid pattern.
	let publish = moq_auth::Pattern::subtree(identity.as_str()).map_err(|_| crate::Error::EmptyIdentity)?;
	Ok(moq_auth::Claims::default()
		.with_root(room)
		.with_subscribe([moq_auth::Pattern::all()])
		.with_publish([publish]))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn patterns(texts: &[&str]) -> moq_auth::Patterns {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	#[test]
	fn rejects_empty_identity() {
		for identity in ["", "/", "///"] {
			assert!(matches!(claims("room", identity), Err(crate::Error::EmptyIdentity)));
		}
	}

	#[test]
	fn claims_root_the_token_and_scope_publish_to_identity() {
		let c = claims("meet/demo", "alice").unwrap();
		assert_eq!(c.root, "meet/demo");
		assert_eq!(c.subscribe, patterns(&["**"]));
		assert_eq!(c.publish, patterns(&["alice/**"]));
	}

	#[test]
	fn claims_does_not_double_a_trailing_slash() {
		assert_eq!(claims("meet/demo", "alice/").unwrap().publish, patterns(&["alice/**"]));
	}

	#[test]
	fn claims_accepts_a_multi_segment_identity() {
		let c = claims("hang/room", "guest/uuid").unwrap();
		assert_eq!(c.root, "hang/room");
		assert_eq!(c.publish, patterns(&["guest/uuid/**"]));
	}
}

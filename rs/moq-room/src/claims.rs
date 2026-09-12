//! The LiveKit AccessToken analogue: minting a moq-token rooted at the room prefix.
//!
//! This crate has no service and no storage. Joining a room is signing a token
//! with these claims and dialing the relay at that root.

/// Token claims for a participant in `room`.
///
/// `root` is the room prefix, `subscribe` is `[""]` (everything under the room),
/// and `publish` is `["<identity>/"]` so a participant cannot publish at anyone
/// else's paths.
pub fn claims(room: impl Into<String>, identity: &str) -> Result<moq_token::Claims, crate::Error> {
	let identity = moq_net::Path::new(identity);
	if identity.is_empty() {
		return Err(crate::Error::EmptyIdentity);
	}
	Ok(moq_token::Claims::default()
		.with_root(room)
		.with_subscribe([""])
		.with_publish([format!("{identity}/")]))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rejects_empty_identity() {
		for identity in ["", "/", "///"] {
			assert!(matches!(claims("room", identity), Err(crate::Error::EmptyIdentity)));
		}
	}

	#[test]
	fn claims_root_the_token_and_scope_put_to_identity() {
		let c = claims("meet/demo", "alice").unwrap();
		assert_eq!(c.root, "meet/demo");
		assert_eq!(c.subscribe, [""]);
		assert_eq!(c.publish, ["alice/"]);
	}

	#[test]
	fn claims_does_not_double_a_trailing_slash() {
		assert_eq!(claims("meet/demo", "alice/").unwrap().publish, ["alice/"]);
	}

	#[test]
	fn claims_accepts_a_multi_segment_identity() {
		let c = claims("hang/room", "guest/uuid").unwrap();
		assert_eq!(c.root, "hang/room");
		assert_eq!(c.publish, ["guest/uuid/"]);
	}
}

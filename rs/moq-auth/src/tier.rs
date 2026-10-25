//! The stats tier a grant or refusal names.

/// Refuse a tier that is not `/`-joined segments of ASCII letters, digits, `.`, `_`, and `-`.
///
/// A relay renders the tier as a metrics label, so this keeps an auth server from
/// forging or breaking a series. The empty tier is the default one.
pub fn validate(tier: &str) -> crate::Result<()> {
	let segment = |segment: &str| {
		!segment.is_empty()
			&& segment
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
	};
	match tier.is_empty() || tier.split('/').all(segment) {
		true => Ok(()),
		false => Err(crate::Error::InvalidTier(tier.to_owned())),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn accepts_slash_joined_names() {
		for tier in ["", "edge", "region/sjc", "a.b_c-D/9"] {
			validate(tier).unwrap();
		}
	}

	#[test]
	fn refuses_anything_a_label_would_have_to_escape() {
		for tier in [
			"a\"b", "a\\b", "a\nb", "a b", "/edge", "edge/", "a//b", "é", "a}b", "a,b", "a=b",
		] {
			assert!(
				matches!(validate(tier), Err(crate::Error::InvalidTier(ref bad)) if bad == tier),
				"{tier:?}"
			);
		}
	}
}

//! Room path convention: identity is everything before the last segment, and
//! that last segment is the broadcast kind (`camera` or `screen`).
//!
//! `alice/camera`, `alice/camera.hang`, and `guest/uuid/screen` are all valid.
//! A `.hang` suffix on the kind is optional and equivalent.

use moq_net::{Path, path};

/// The two broadcasts each participant may publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
	/// Camera plus microphone.
	Camera,
	/// Screenshare; its announce/unannounce is the share lifecycle.
	Screen,
}

impl Kind {
	/// The path segment this kind publishes as, without a catalog-format suffix.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Camera => "camera",
			Self::Screen => "screen",
		}
	}
}

/// An announced path split into identity and kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
	/// Path prefix identifying the participant; may be more than one segment.
	pub identity: Path,
	/// `camera` (camera + mic) or `screen` (screenshare).
	pub kind: Kind,
}

/// Strip an optional `.hang` catalog-format suffix from a path segment.
pub fn kind_from_segment(segment: &str) -> Option<Kind> {
	let kind = segment.strip_suffix(".hang").unwrap_or(segment);
	match kind {
		"camera" => Some(Kind::Camera),
		"screen" => Some(Kind::Screen),
		_ => None,
	}
}

/// Split a room-relative broadcast path into identity and kind.
///
/// Returns `None` when there is no identity, the last segment is not a kind, or the
/// `/` before it is escaped.
pub fn parse(path: impl Into<Path>) -> Option<Parsed> {
	let path: Path = path.into();
	let count = path.parts().count();
	if count < 2 {
		return None;
	}
	let kind = kind_from_segment(path.parts().last()?)?;
	let identity = path.head(count - 1)?;
	Some(Parsed { identity, kind })
}

/// The broadcast path a participant publishes for `kind`, failing if it is too long.
pub fn broadcast_path(identity: impl Into<Path>, kind: Kind) -> Result<Path, path::Error> {
	let identity: Path = identity.into();
	identity.join(&Path::new(&format!("{}.hang", kind.as_str()))?)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn p(s: &str) -> Path {
		Path::new(s).unwrap()
	}

	#[test]
	fn parse_splits_identity_and_kind() {
		assert_eq!(
			parse(Path::from_static("alice/camera")),
			Some(Parsed {
				identity: p("alice"),
				kind: Kind::Camera
			})
		);
		assert_eq!(
			parse(Path::from_static("alice/screen")),
			Some(Parsed {
				identity: p("alice"),
				kind: Kind::Screen
			})
		);
	}

	#[test]
	fn parse_accepts_hang_suffix() {
		assert_eq!(
			parse(Path::from_static("alice/camera.hang")).map(|p| p.kind),
			Some(Kind::Camera)
		);
		assert_eq!(
			parse(Path::from_static("alice/screen.hang")).map(|p| p.kind),
			Some(Kind::Screen)
		);
	}

	#[test]
	fn parse_keeps_multi_segment_identity() {
		assert_eq!(
			parse(Path::from_static("guest/uuid/camera")),
			Some(Parsed {
				identity: p("guest/uuid"),
				kind: Kind::Camera
			})
		);
	}

	#[test]
	fn parse_rejects_unknown() {
		assert_eq!(parse(Path::from_static("alice")), None);
		assert_eq!(parse(Path::from_static("alice/chat")), None);
		assert_eq!(parse(Path::from_static("camera")), None);
		assert_eq!(parse(Path::empty()), None);
		// A peer's escaped `/` is not the separator before a kind.
		assert_eq!(parse(Path::from_static(r"alice\/camera")), None);
	}

	#[test]
	fn broadcast_path_joins_with_hang_suffix() {
		assert_eq!(
			broadcast_path(p("alice"), Kind::Camera).unwrap().as_str(),
			"alice/camera.hang"
		);
		assert_eq!(
			broadcast_path(p("guest/uuid"), Kind::Screen).unwrap().as_str(),
			"guest/uuid/screen.hang"
		);
	}
}

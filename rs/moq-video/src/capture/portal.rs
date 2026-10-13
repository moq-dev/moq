//! A system-picked source and the grant owned by one logical publication.

use std::fmt;
use std::sync::{Arc, Mutex};

/// The type of source the user may choose in the system picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	/// One whole screen.
	Screen,
	/// One application window.
	Window,
}

impl Kind {
	pub(crate) fn bits(self) -> u32 {
		match self {
			Self::Screen => 1,
			Self::Window => 2,
		}
	}

	pub(crate) fn validate(self, available: u32) -> Result<(), &'static str> {
		if available & self.bits() == 0 {
			return Err(match self {
				Self::Screen => "The system portal does not support screen sharing.",
				Self::Window => "The system portal does not support window sharing.",
			});
		}
		Ok(())
	}

	pub(crate) fn validate_stream(self, version: u32, count: usize, source: Option<u32>) -> Result<(), &'static str> {
		if count != 1 {
			return Err("The system portal must grant exactly one source.");
		}
		// ScreenCast v1/v2 omit source_type; SelectSources still constrains the kind.
		if source.is_none() && version < 3 {
			return Ok(());
		}
		if source != Some(self.bits()) {
			return Err("The system portal did not confirm the requested source type.");
		}
		Ok(())
	}
}

/// The source and restore token returned by a successful portal Start.
pub(crate) struct Grant<'a> {
	pub streams: usize,
	pub source: Option<u32>,
	pub token: Option<&'a str>,
}

/// One user's selection, shared only across automatic reopens of that publication.
#[derive(Clone)]
pub struct Selection {
	kind: Kind,
	restore: Arc<Mutex<Option<String>>>,
}

impl Selection {
	/// Create a selection without a grant from an earlier publication.
	pub fn new(kind: Kind) -> Self {
		Self {
			kind,
			restore: Arc::default(),
		}
	}

	pub(crate) fn kind(&self) -> Kind {
		self.kind
	}

	// Restore tokens are single-use, including when the next request fails.
	pub(crate) fn take_restore(&self) -> Option<String> {
		self.restore.lock().unwrap().take()
	}

	// Commit a validated grant before opening the remote can suspend or be cancelled.
	pub(crate) fn accept(&self, version: u32, grant: Grant<'_>) -> Result<(), &'static str> {
		self.kind.validate_stream(version, grant.streams, grant.source)?;
		self.replace_restore(grant.token.map(str::to_string));
		Ok(())
	}

	pub(crate) fn replace_restore(&self, token: Option<String>) {
		*self.restore.lock().unwrap() = token;
	}
}

impl fmt::Debug for Selection {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		// Grants must not appear in source-selection logs.
		f.debug_struct("Selection")
			.field("kind", &self.kind)
			.finish_non_exhaustive()
	}
}

impl PartialEq for Selection {
	fn eq(&self, other: &Self) -> bool {
		self.kind == other.kind && Arc::ptr_eq(&self.restore, &other.restore)
	}
}
impl Eq for Selection {}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn demand_reopen_keeps_only_this_publications_grant() {
		let selection = Selection::new(Kind::Window);
		let reopened = selection.clone();
		selection.replace_restore(Some("first".into()));
		assert_eq!(reopened.take_restore().as_deref(), Some("first"));
		assert_eq!(selection.take_restore(), None);
		reopened.replace_restore(Some("replacement".into()));
		assert_eq!(selection.take_restore().as_deref(), Some("replacement"));
		assert_eq!(selection, reopened);
	}

	#[test]
	fn cancelling_remote_open_keeps_the_validated_replacement() {
		use std::future::{Future, pending};
		use std::task::{Context, Poll, Waker};

		let selection = Selection::new(Kind::Window);
		selection.replace_restore(Some("old".into()));
		assert_eq!(selection.take_restore().as_deref(), Some("old"));
		let mut opening = Box::pin(async {
			// Start has returned; opening the remote has not completed yet.
			selection.accept(
				4,
				Grant {
					streams: 1,
					source: Some(2),
					token: Some("replacement"),
				},
			)?;
			pending::<Result<(), &'static str>>().await
		});
		assert_eq!(
			opening.as_mut().poll(&mut Context::from_waker(Waker::noop())),
			Poll::Pending
		);
		drop(opening);

		assert_eq!(selection.clone().take_restore().as_deref(), Some("replacement"));
		assert_eq!(selection.take_restore(), None);
	}

	#[test]
	fn rejected_source_does_not_save_a_restore_grant() {
		let selection = Selection::new(Kind::Window);
		assert!(
			selection
				.accept(
					4,
					Grant {
						streams: 1,
						source: Some(1),
						token: Some("wrong-screen")
					}
				)
				.is_err()
		);
		assert_eq!(selection.take_restore(), None);
	}

	#[test]
	fn new_share_reselects_even_when_the_source_type_is_unchanged() {
		let old = Selection::new(Kind::Window);
		old.replace_restore(Some("old-window".into()));
		let new = Selection::new(Kind::Window);
		assert_ne!(old, new);
		assert_eq!(new.take_restore(), None);
		assert_eq!(Selection::new(Kind::Screen).take_restore(), None);
	}

	#[test]
	fn old_source_failure_cannot_clear_a_new_publications_grant() {
		let old = Selection::new(Kind::Window);
		let new = Selection::new(Kind::Window);
		new.replace_restore(Some("new-window".into()));
		old.replace_restore(None);
		assert_eq!(new.take_restore().as_deref(), Some("new-window"));
	}

	#[test]
	fn missing_window_support_never_falls_back_to_a_screen() {
		assert!(Kind::Window.validate(1).is_err());
		assert!(Kind::Screen.validate(2).is_err());
		assert!(Kind::Window.validate(3).is_ok());
		assert!(Kind::Screen.validate(3).is_ok());
		assert!(Kind::Window.validate(0).is_err());
	}

	#[test]
	fn mismatched_or_ambiguous_grants_are_rejected() {
		assert!(Kind::Window.validate_stream(3, 1, Some(2)).is_ok());
		assert!(Kind::Screen.validate_stream(3, 1, Some(1)).is_ok());
		assert!(Kind::Window.validate_stream(3, 1, Some(1)).is_err());
		assert!(Kind::Window.validate_stream(3, 1, None).is_err());
		assert!(Kind::Window.validate_stream(3, 0, None).is_err());
		assert!(Kind::Window.validate_stream(3, 2, Some(2)).is_err());
	}

	#[test]
	fn older_portals_can_omit_source_type() {
		for version in [1, 2] {
			for kind in [Kind::Screen, Kind::Window] {
				let selection = Selection::new(kind);
				assert!(
					selection
						.accept(
							version,
							Grant {
								streams: 1,
								source: None,
								token: None
							}
						)
						.is_ok()
				);
				assert_eq!(selection.take_restore(), None);
			}
		}
	}

	#[test]
	fn portal_versions_do_not_relax_mismatched_or_multiple_sources() {
		for version in [1, 2, 3, 4, 6] {
			assert!(Kind::Window.validate_stream(version, 1, Some(1)).is_err());
			assert!(Kind::Screen.validate_stream(version, 1, Some(2)).is_err());
			assert!(Kind::Window.validate_stream(version, 0, None).is_err());
			assert!(Kind::Window.validate_stream(version, 2, Some(2)).is_err());
			assert!(Kind::Window.validate_stream(version, 1, Some(2)).is_ok());
			if version >= 3 {
				assert!(Kind::Window.validate_stream(version, 1, None).is_err());
			}
		}
	}

	#[test]
	fn diagnostic_output_does_not_expose_the_grant() {
		let selection = Selection::new(Kind::Window);
		selection.replace_restore(Some("private-grant".into()));
		assert!(!format!("{selection:?}").contains("private-grant"));
		assert_eq!(selection.kind(), Kind::Window);
	}
}

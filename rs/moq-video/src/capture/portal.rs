//! A system-picked source and the grant owned by one logical publication.

use std::fmt;
use std::task::Poll;

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

/// A portal grant's restore token, which reopens its source without asking the user.
///
/// Store it like a credential. `Debug` redacts it so it never reaches a log.
#[derive(Clone, PartialEq, Eq)]
pub struct RestoreToken(String);

impl RestoreToken {
	/// Wrap a token persisted from an earlier [`RestoreToken::as_str`].
	pub fn new(token: impl Into<String>) -> Self {
		Self(token.into())
	}

	/// The token, for the caller to persist.
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl fmt::Debug for RestoreToken {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("RestoreToken(..)")
	}
}

/// One user's selection, shared only across automatic reopens of that publication.
#[derive(Clone)]
pub struct Selection {
	kind: Kind,
	restore: kio::Producer<Option<RestoreToken>>,
}

impl Selection {
	/// Create a selection without a grant from an earlier publication.
	pub fn new(kind: Kind) -> Self {
		Self {
			kind,
			restore: kio::Producer::default(),
		}
	}

	/// Seed the selection with a token persisted by an earlier process, so the
	/// first open can skip the picker on a compositor that keeps grants.
	pub fn with_restore_token(self, token: RestoreToken) -> Self {
		self.replace_restore(Some(token));
		self
	}

	/// Watch the restore token, to persist each change.
	///
	/// The token is single-use: an open takes it before asking the portal, and
	/// stores the replacement once the portal grants a source. It is `None`
	/// before a grant, while an open holds it, after an open that failed before
	/// a grant, on a compositor that does not persist grants, and once the
	/// producer ends the stream (the user stopped sharing, the stream failed, or
	/// the source went away). Persist every change and delete the stored copy
	/// on `None`, since the crate would not use that copy either.
	pub fn restore_token(&self) -> RestoreTokenConsumer {
		let state = self.restore.consume();
		let seen = state.read().clone();
		RestoreTokenConsumer { state, seen }
	}

	pub(crate) fn kind(&self) -> Kind {
		self.kind
	}

	// Restore tokens are single-use, including when the next request fails.
	pub(crate) fn take_restore(&self) -> Option<RestoreToken> {
		self.replace_restore(None)
	}

	// Commit a validated grant before opening the remote can suspend or be cancelled.
	pub(crate) fn accept(&self, version: u32, grant: Grant<'_>) -> Result<(), &'static str> {
		self.kind.validate_stream(version, grant.streams, grant.source)?;
		self.replace_restore(grant.token.map(RestoreToken::new));
		Ok(())
	}

	// Store `token` and return the previous one, waking watchers only on a change.
	pub(crate) fn replace_restore(&self, token: Option<RestoreToken>) -> Option<RestoreToken> {
		// Nothing closes the channel, and every `Selection` holds a producer.
		let Ok(mut current) = self.restore.write() else {
			return None;
		};
		if *current == token {
			return token;
		}
		std::mem::replace(&mut *current, token)
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
		self.kind == other.kind && self.restore.same_channel(&other.restore)
	}
}
impl Eq for Selection {}

/// Watches a [`Selection`]'s restore token without keeping the selection alive.
#[derive(Clone)]
pub struct RestoreTokenConsumer {
	state: kio::Consumer<Option<RestoreToken>>,
	seen: Option<RestoreToken>,
}

impl RestoreTokenConsumer {
	/// Return the current token without waiting.
	pub fn token(&self) -> Option<RestoreToken> {
		self.state.read().clone()
	}

	/// Wait for the token to differ from the last one this consumer saw and
	/// return it, `Some(None)` once cleared, or `None` after every clone of the
	/// selection is dropped. Changes in between are coalesced into the latest.
	pub async fn changed(&mut self) -> Option<Option<RestoreToken>> {
		let seen = &self.seen;
		let token = self
			.state
			.wait(|current| match **current != *seen {
				true => Poll::Ready((**current).clone()),
				false => Poll::Pending,
			})
			.await
			.ok()?;
		self.seen = token.clone();
		Some(token)
	}
}

impl fmt::Debug for RestoreTokenConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("RestoreTokenConsumer")
			.field("token", &*self.state.read())
			.finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn demand_reopen_keeps_only_this_publications_grant() {
		let selection = Selection::new(Kind::Window);
		let reopened = selection.clone();
		selection.replace_restore(token("first"));
		assert_eq!(reopened.take_restore(), token("first"));
		assert_eq!(selection.take_restore(), None);
		reopened.replace_restore(token("replacement"));
		assert_eq!(selection.take_restore(), token("replacement"));
		assert_eq!(selection, reopened);
	}

	#[test]
	fn cancelling_remote_open_keeps_the_validated_replacement() {
		use std::future::{Future, pending};
		use std::task::{Context, Poll, Waker};

		let selection = Selection::new(Kind::Window);
		selection.replace_restore(token("old"));
		assert_eq!(selection.take_restore(), token("old"));
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

		assert_eq!(selection.clone().take_restore(), token("replacement"));
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
		old.replace_restore(token("old-window"));
		let new = Selection::new(Kind::Window);
		assert_ne!(old, new);
		assert_eq!(new.take_restore(), None);
		assert_eq!(Selection::new(Kind::Screen).take_restore(), None);
	}

	#[test]
	fn old_source_failure_cannot_clear_a_new_publications_grant() {
		let old = Selection::new(Kind::Window);
		let new = Selection::new(Kind::Window);
		new.replace_restore(token("new-window"));
		old.replace_restore(None);
		assert_eq!(new.take_restore(), token("new-window"));
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
	fn a_watcher_sees_every_token_a_caller_must_persist() {
		let selection = Selection::new(Kind::Screen).with_restore_token(RestoreToken::new("saved"));
		let mut watch = selection.restore_token();
		assert_eq!(watch.token(), token("saved"));
		assert!(changed(&mut watch).is_pending());

		// An open takes the seed, so a stored copy is dead even if the open fails.
		assert_eq!(selection.clone().take_restore(), token("saved"));
		assert_eq!(changed(&mut watch), Poll::Ready(Some(None)));
		selection.accept(4, grant("granted")).unwrap();
		assert_eq!(changed(&mut watch), Poll::Ready(Some(token("granted"))));

		// Storing the same token is not a change.
		selection.replace_restore(token("granted"));
		assert!(changed(&mut watch).is_pending());

		// A reopen the caller did not observe still ends at the newest grant.
		selection.take_restore();
		selection.accept(4, grant("rotated")).unwrap();
		assert_eq!(changed(&mut watch), Poll::Ready(Some(token("rotated"))));

		// The producer ended the stream.
		selection.replace_restore(None);
		assert_eq!(changed(&mut watch), Poll::Ready(Some(None)));

		// The watcher does not keep the selection alive.
		drop(selection);
		assert_eq!(changed(&mut watch), Poll::Ready(None));
	}

	#[test]
	fn diagnostic_output_does_not_expose_the_grant() {
		let selection = Selection::new(Kind::Window);
		selection.replace_restore(token("private-grant"));
		assert!(!format!("{selection:?}").contains("private-grant"));
		assert!(!format!("{:?}", selection.restore_token()).contains("private-grant"));
		assert!(!format!("{:?}", RestoreToken::new("private-grant")).contains("private-grant"));
		assert_eq!(selection.kind(), Kind::Window);
	}

	fn token(token: &str) -> Option<RestoreToken> {
		Some(RestoreToken::new(token))
	}

	fn grant(token: &str) -> Grant<'_> {
		Grant {
			streams: 1,
			source: Some(Kind::Screen.bits()),
			token: Some(token),
		}
	}

	fn changed(watch: &mut RestoreTokenConsumer) -> Poll<Option<Option<RestoreToken>>> {
		use std::future::Future;
		let mut changed = Box::pin(watch.changed());
		changed
			.as_mut()
			.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
	}
}

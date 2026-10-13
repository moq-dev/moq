//! The MAX_REQUEST_UPDATES Setup Option (draft-ietf-moq-transport-19 section 10.3.1.7).
//!
//! A request stream (SUBSCRIBE, PUBLISH_NAMESPACE, ...) can be refreshed in place with
//! REQUEST_UPDATE messages. Each is outstanding from when the peer sends it until its
//! REQUEST_OK or REQUEST_ERROR, and MAX_REQUEST_UPDATES caps how many a peer may leave
//! outstanding on one stream at once. We verify a request token asynchronously, so a slow
//! acceptor holds one renewal unanswered while more pile up behind it; this bound is what
//! stops that queue from growing without limit.
//!
//! The option is draft-19+ (Option Type 0x08). Advertising it lets a conforming peer
//! self-limit, so a peer that exceeds the advertised value broke a negotiated rule, which
//! the draft answers with a session close ([`SessionError::TooManyRequestUpdates`]). Drafts
//! below 19 carry no such option, so a peer there agreed to no ceiling: the receiver keeps
//! a generous local guard that ends only the one request rather than fault a peer at
//! session scope for a limit it never saw.
//!
//! [`SessionError::TooManyRequestUpdates`]: crate::SessionError::TooManyRequestUpdates

use std::{collections::VecDeque, task::Poll};

use super::{RequestId, Version};
use crate::{Error, SessionError, auth};

/// MAX_REQUEST_UPDATES Setup Option (Option Type 0x08). Even, so the value is a bare varint.
pub const OPTION: u64 = 0x08;

/// The credit we advertise and enforce on drafts that define the option: the most
/// unacknowledged REQUEST_UPDATEs we accept on one request stream, counting the one being
/// verified. One constant drives both the advertisement and the enforcement so the value we
/// promise and the value we hold a peer to cannot drift.
pub const MAX_REQUEST_UPDATES: u64 = 16;

/// Local resource guard on drafts without the option. Generous relative to the negotiated
/// credit, since those drafts negotiate no limit and a conforming peer must never reach it, but
/// still bounded: a control message carries up to a 16-bit length, so this caps the buffered
/// bytes per request stream rather than letting a flood grow without end. Exceeding it ends the
/// one request, never the session.
pub const UNNEGOTIATED_GUARD: usize = 64;

/// Whether this version defines MAX_REQUEST_UPDATES. Draft-19 section 10.3.1.7 added it; the
/// drafts below carry no such Setup Option.
pub fn supported(version: Version) -> bool {
	!matches!(
		version,
		Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 | Version::Draft18
	)
}

/// A request's in-band renewal: the verdict on the token being verified, and the updates that
/// arrived behind it.
///
/// From draft 17 an answer names no Request ID, so updates are answered in arrival order: one
/// behind a pending verdict waits here until the verdict ahead of it resolves. The queue is
/// bounded by MAX_REQUEST_UPDATES, counting the one being verified.
pub(super) struct Renewal<T> {
	pending: Option<(auth::RequestVerdict, RequestId)>,
	queued: VecDeque<T>,
}

impl<T> Default for Renewal<T> {
	fn default() -> Self {
		Self {
			pending: None,
			queued: VecDeque::new(),
		}
	}
}

/// An update past the bound on outstanding updates.
pub(super) enum Overflow {
	/// Past the MAX_REQUEST_UPDATES we advertised (draft 19+): the session closes.
	Session,
	/// Past the local guard on a draft without the option: only the request ends.
	Request,
}

impl<T> Renewal<T> {
	/// Whether a verdict is pending, so a new update must wait behind it.
	pub fn pending(&self) -> bool {
		self.pending.is_some()
	}

	/// Wait on `verdict`, the renewal the update with `request_id` carried.
	pub fn verify(&mut self, verdict: auth::RequestVerdict, request_id: RequestId) {
		debug_assert!(self.pending.is_none(), "one renewal is verified at a time");
		self.pending = Some((verdict, request_id));
	}

	/// Queue `update` behind the pending verdict, counting it against the bound.
	pub fn queue(&mut self, update: T, version: Version) -> Result<(), Overflow> {
		// Outstanding counts the one being verified plus those already queued.
		let outstanding = self.queued.len() as u64 + 1;
		match supported(version) {
			// We advertised the credit, so a peer with that many outstanding sending another
			// broke the negotiated limit (draft-19 section 10.3.1.7).
			true if outstanding >= MAX_REQUEST_UPDATES => return Err(Overflow::Session),
			false if self.queued.len() >= UNNEGOTIATED_GUARD => return Err(Overflow::Request),
			_ => {}
		}
		self.queued.push_back(update);
		Ok(())
	}

	/// The next queued update, once no verdict is pending.
	pub fn next(&mut self) -> Option<T> {
		match self.pending {
			Some(_) => None,
			None => self.queued.pop_front(),
		}
	}

	/// `Ready` with the pending verdict once it resolves.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Verdict> {
		let Some((verdict, _)) = &self.pending else {
			return Poll::Pending;
		};
		let grant = std::task::ready!(verdict.poll_grant(waiter));
		let (verdict, request_id) = self.pending.take().expect("a pending renewal");
		Poll::Ready(Verdict {
			verdict,
			grant,
			request_id,
		})
	}
}

/// A renewal's resolved verdict.
pub(super) struct Verdict {
	/// What renews the request grant, if the grant covers it.
	pub verdict: auth::RequestVerdict,
	/// The acceptor's grant, or its refusal.
	pub grant: crate::Result<auth::Grant>,
	/// The update that carried the renewal, which the answer names before draft 17.
	pub request_id: RequestId,
}

/// Close `session` for a peer past the MAX_REQUEST_UPDATES we advertised, returning the error
/// that ends the request.
pub(super) fn too_many<S: crate::transport::poll::Session>(session: &mut S) -> Error {
	let err = SessionError::TooManyRequestUpdates;
	session.close(err.to_code(), "too many request updates");
	Error::Session(err)
}

/// Advertise our MAX_REQUEST_UPDATES credit, on versions that define the option.
pub fn into_setup(params: &mut super::Parameters, version: Version) {
	if supported(version) {
		params.set_varint(super::ParameterVarInt::MaxRequestUpdates, MAX_REQUEST_UPDATES);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The option rides SETUP only on the drafts that define it; an older peer sees nothing,
	/// which is what keeps the bound a negotiated contract rather than a surprise.
	#[test]
	fn advertised_only_on_draft_19_plus() {
		for version in [Version::Draft19, Version::Draft20, Version::Draft21, Version::Draft22] {
			let mut params = super::super::Parameters::default();
			into_setup(&mut params, version);
			assert_eq!(
				params.get_varint(super::super::ParameterVarInt::MaxRequestUpdates),
				Some(MAX_REQUEST_UPDATES),
				"{version} must advertise MAX_REQUEST_UPDATES"
			);
		}

		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			let mut params = super::super::Parameters::default();
			into_setup(&mut params, version);
			assert_eq!(
				params.get_varint(super::super::ParameterVarInt::MaxRequestUpdates),
				None,
				"{version} has no MAX_REQUEST_UPDATES option to advertise"
			);
		}
	}
}

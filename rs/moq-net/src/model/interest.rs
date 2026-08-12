//! Lazy announce solicitation: tracking who wants announcements so sessions only
//! ask their peer while someone is listening.
//!
//! One [`Ledger`] is shared by every handle derived from an origin. Two sides
//! write to it:
//!
//! - **Demand.** Every `AnnounceConsumer` registers the prefixes it listens to (a
//!   [`Guard`], refcounted, released on drop). That covers `announced()`,
//!   `announced_broadcast`, the wait inside `request_broadcast`, and a session's
//!   egress consumer serving its peer's solicitation. The last one is tagged with
//!   the session's [`SessionId`]; everything else is the application's own
//!   (`None`).
//! - **Supply.** Each session's subscriber half attaches a [`Solicitor`]. It
//!   parks until demand it should serve exists ([`Solicitor::needed`]), then
//!   solicits announces from its peer for the rest of the session, reporting
//!   [`Solicitor::synced`] once the initial announce set has landed (or provably
//!   never will).
//!
//! The session tag is what breaks the feedback loop: a solicitor never counts
//! demand raised on behalf of its own peer, so a peer soliciting our announces
//! doesn't make us solicit theirs right back, and a publish-only endpoint keeps
//! the traffic savings laziness exists for.
//!
//! A pending `request_broadcast` consumes the sync half: it waits until every
//! solicitor that could announce its path is [`settled`](Ledger::settled) before
//! falling back to the dynamic queue or `Unroutable`.

use std::{
	collections::{BTreeMap, HashMap},
	sync::atomic::{AtomicU64, Ordering},
	task::Poll,
};

use crate::{Path, PathOwned};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(0);

/// A session's identity in the announce-interest [`Ledger`].
///
/// Minted once per session and shared by both halves: the publisher half tags
/// its egress consumer with it (interest that consumer raises is on behalf of
/// the session's peer), and the subscriber half attaches its [`Solicitor`] with
/// it, so the peer's own solicitation never reads as demand to reciprocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SessionId(u64);

impl SessionId {
	/// A fresh identity for one session.
	pub fn new() -> Self {
		Self(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed))
	}
}

// The ledger's record of one attached solicitor: which session it belongs to,
// the prefixes it may solicit, and whether its initial announce set has been
// delivered (or definitively won't be, e.g. the stream failed or the peer is
// draining).
struct SolicitorEntry {
	session: SessionId,
	synced: bool,
	// The solicitor's authorized scope, absolute. It can only ever announce paths
	// under one of these, which is what keeps an unrelated session out of another's
	// requests.
	prefixes: Vec<PathOwned>,
}

impl SolicitorEntry {
	// Whether this solicitor could announce `path`: some prefix it may solicit contains it.
	fn covers(&self, path: &Path) -> bool {
		self.prefixes.iter().any(|prefix| path.has_prefix(prefix))
	}
}

// Whether two prefixes can name any path in common: one contains the other. Interest in
// `a/b` is served by a solicitor scoped to `a`, and interest in `a` is (partly) served by
// a solicitor scoped to `a/b`; interest in `b` is served by neither.
fn overlaps(a: &Path, b: &Path) -> bool {
	a.has_prefix(b) || b.has_prefix(a)
}

/// Announce demand across an origin, shared by every handle derived from it; see
/// the module docs for the full story.
#[derive(Default)]
pub(crate) struct Ledger {
	// Absolute prefix -> who raised the interest -> registration count. `None` is
	// the application itself; `Some` is a session's egress consumer, acting on
	// behalf of that session's peer.
	prefixes: BTreeMap<PathOwned, BTreeMap<Option<SessionId>, usize>>,

	// Attached solicitors, keyed by an id private to this map.
	solicitors: HashMap<u64, SolicitorEntry>,
	next_solicitor: u64,
}

impl Ledger {
	fn register(&mut self, prefix: PathOwned, session: Option<SessionId>) {
		*self.prefixes.entry(prefix).or_default().entry(session).or_default() += 1;
	}

	fn unregister(&mut self, prefix: &PathOwned, session: Option<SessionId>) {
		let Some(parties) = self.prefixes.get_mut(prefix) else {
			return;
		};
		let Some(count) = parties.get_mut(&session) else { return };
		*count -= 1;
		if *count == 0 {
			parties.remove(&session);
			if parties.is_empty() {
				self.prefixes.remove(prefix);
			}
		}
	}

	/// Whether any interest exists that `solicitor` should serve: raised by the
	/// application or on behalf of a different session, and under a prefix it may
	/// actually solicit. A session never solicits announces just because its own
	/// peer solicited ours, and interest in a path outside a solicitor's scope is
	/// not that solicitor's to answer.
	fn needs(&self, solicitor: &SolicitorEntry) -> bool {
		self.prefixes.iter().any(|(prefix, parties)| {
			parties.keys().any(|s| *s != Some(solicitor.session))
				&& solicitor.prefixes.iter().any(|scope| overlaps(prefix, scope))
		})
	}

	/// Whether the announce table is as warm as the attached sessions will make it
	/// *for `path`*: every solicitor that could announce it has delivered its
	/// initial set (or has no interest to serve, so it never will). Scoped per path
	/// rather than globally, because a session scoped to somewhere else can neither
	/// announce this path nor speak for it, so waiting on one would hang a request
	/// it can never answer.
	///
	/// Vacuously true with no attached sessions, so a purely local origin resolves
	/// requests immediately, exactly as before interest existed.
	pub fn settled(&self, path: &Path) -> bool {
		self.solicitors
			.values()
			.filter(|s| s.covers(path))
			.all(|s| s.synced || !self.needs(s))
	}

	/// The registered prefixes, for tests asserting that registration is absolute.
	#[cfg(test)]
	pub fn prefixes(&self) -> Vec<PathOwned> {
		self.prefixes.keys().cloned().collect()
	}

	/// Whether any attached solicitor might still warm `path`. Broader than
	/// `!settled()`: a request consults this *before* registering its own interest,
	/// so a parked solicitor that the interest is about to wake still counts.
	pub fn warming(&self, path: &Path) -> bool {
		self.solicitors.values().any(|s| !s.synced && s.covers(path))
	}
}

/// Holds announce-interest registrations for one holder (an `AnnounceConsumer`,
/// or the origin-wide `origin::Interest`); dropping it releases them.
pub(crate) struct Guard {
	state: kio::Producer<Ledger>,
	// Absolute prefixes this consumer registered, one per scoped root.
	prefixes: Vec<PathOwned>,
	// Who the interest is on behalf of: `None` for the application itself.
	session: Option<SessionId>,
}

impl Guard {
	pub fn register(state: kio::Producer<Ledger>, prefixes: Vec<PathOwned>, session: Option<SessionId>) -> Self {
		if let Ok(mut locked) = state.write() {
			for prefix in &prefixes {
				locked.register(prefix.clone(), session);
			}
		}
		Self {
			state,
			prefixes,
			session,
		}
	}
}

impl Drop for Guard {
	fn drop(&mut self) {
		if let Ok(mut locked) = self.state.write() {
			for prefix in &self.prefixes {
				locked.unregister(prefix, self.session);
			}
		}
	}
}

/// A session subscriber half's attachment to the announce-interest [`Ledger`].
///
/// Obtained via `origin::Producer::solicitor`. The solicitor waits on
/// [`poll_needed`](Self::poll_needed) before soliciting announces, then calls
/// [`synced`](Self::synced) once the initial announce set has landed (or provably
/// never will). Dropping it detaches, so a dead session can't hold requests
/// hostage.
pub(crate) struct Solicitor {
	state: kio::Producer<Ledger>,
	id: u64,
}

impl Solicitor {
	pub fn attach(state: kio::Producer<Ledger>, session: SessionId, prefixes: Vec<PathOwned>) -> Self {
		let id = {
			let mut locked = state.write().ok().expect("interest never closes");
			let id = locked.next_solicitor;
			locked.next_solicitor += 1;
			locked.solicitors.insert(
				id,
				SolicitorEntry {
					session,
					synced: false,
					prefixes,
				},
			);
			id
		};
		Self { state, id }
	}

	/// Whether interest this solicitor should serve already exists, without waiting.
	#[cfg(test)]
	pub fn is_needed(&self) -> bool {
		let state = self.state.read();
		state.solicitors.get(&self.id).is_some_and(|s| state.needs(s))
	}

	/// Poll until some interest this solicitor should serve exists.
	pub fn poll_needed(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state
			.poll_ref(waiter, |state| match state.solicitors.get(&self.id) {
				Some(solicitor) if state.needs(solicitor) => Poll::Ready(()),
				_ => Poll::Pending,
			})
			.map(|_| ())
	}

	/// Block until some interest this solicitor should serve exists.
	pub async fn needed(&self) {
		kio::wait(|waiter| self.poll_needed(waiter)).await
	}

	/// Report that this solicitor's initial announce set has been delivered, or
	/// never will be (a failed stream, a draining peer): pending requests stop
	/// waiting on it.
	pub fn synced(&self) {
		if let Ok(mut locked) = self.state.write()
			&& let Some(entry) = locked.solicitors.get_mut(&self.id)
		{
			entry.synced = true;
		}
	}
}

impl Drop for Solicitor {
	fn drop(&mut self) {
		if let Ok(mut locked) = self.state.write() {
			locked.solicitors.remove(&self.id);
		}
	}
}
